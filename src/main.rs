use std::{collections::HashMap, env::args, fs::File, io::Write};
use walrus::{
    ExportItem, FunctionBuilder, FunctionId, FunctionKind, InstrSeqBuilder, LocalFunction, LocalId,
    Module, ModuleConfig, ModuleLocals, ValType,
    ir::{self, InstrSeq, InstrSeqId},
};

fn find_block(stack: &[(InstrSeqId, InstrSeqId)], id: InstrSeqId) -> Option<InstrSeqId> {
    stack.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
}

struct Maps {
    funcs: HashMap<FunctionId, FunctionId>,
    locals: HashMap<LocalId, LocalId>,
    ctx_set: FunctionId,
    ctx_get: FunctionId,
}

fn copy_seq(
    func: &LocalFunction,
    mut ctx: LocalId,
    stack: &mut Vec<(InstrSeqId, InstrSeqId)>,
    maps: &Maps,
    seq: &InstrSeq,
    builder: &mut InstrSeqBuilder,
    locals: &mut ModuleLocals,
) -> InstrSeqId {
    stack.push((seq.id(), builder.id()));
    for (instr, _) in &seq.instrs {
        if instr.is_br() {
            let i = instr.unwrap_br();
            let block = find_block(stack, i.block).unwrap();
            builder.br(block);
        } else if instr.is_br_if() {
            let i = instr.unwrap_br_if();
            let block = find_block(stack, i.block).unwrap();
            builder.br_if(block);
        } else if instr.is_br_table() {
            let i = instr.unwrap_br_table();
            let blocks = i
                .blocks
                .iter()
                .map(|block| find_block(stack, *block))
                .collect::<Option<Box<_>>>()
                .unwrap();
            let default = find_block(stack, i.default).unwrap();
            builder.br_table(blocks, default);
        // =================================================
        } else if instr.is_block() {
            let i = instr.unwrap_block();
            let block = func.block(i.seq);
            let mut block_builder = builder.dangling_instr_seq(block.ty);
            let seq = copy_seq(func, ctx, stack, maps, block, &mut block_builder, locals);
            builder.instr(ir::Block { seq });
        } else if instr.is_loop() {
            let i = instr.unwrap_loop();
            let block = func.block(i.seq);
            let mut block_builder = builder.dangling_instr_seq(block.ty);
            let seq = copy_seq(func, ctx, stack, maps, block, &mut block_builder, locals);
            builder.instr(ir::Loop { seq });
        } else if instr.is_if_else() {
            let i = instr.unwrap_if_else();
            let consequent = func.block(i.consequent);
            let mut consequent_builder = builder.dangling_instr_seq(consequent.ty);
            let consequent = copy_seq(
                func,
                ctx,
                stack,
                maps,
                consequent,
                &mut consequent_builder,
                locals,
            );
            let alternative = func.block(i.alternative);
            let mut alternative_builder = builder.dangling_instr_seq(alternative.ty);
            let alternative = copy_seq(
                func,
                ctx,
                stack,
                maps,
                alternative,
                &mut alternative_builder,
                locals,
            );
            builder.instr(ir::IfElse {
                consequent,
                alternative,
            });
        // =================================================
        } else if instr.is_local_get() {
            let i = instr.unwrap_local_get();
            let local = maps.locals.get(&i.local).unwrap();
            builder.local_get(*local);
        } else if instr.is_local_set() {
            let i = instr.unwrap_local_set();
            let local = maps.locals.get(&i.local).unwrap();
            builder.local_set(*local);
        } else if instr.is_local_tee() {
            let i = instr.unwrap_local_tee();
            let local = maps.locals.get(&i.local).unwrap();
            builder.local_tee(*local);
        // =================================================
        } else if instr.is_ref_func() {
            let i = instr.unwrap_ref_func();
            let func = maps.funcs.get(&i.func).unwrap();
            builder.ref_func(*func);
        } else if instr.is_call() {
            let i = instr.unwrap_call();
            if i.func == maps.ctx_get {
                builder.local_get(ctx);
            } else if i.func == maps.ctx_set {
                ctx = locals.add(ValType::I64);
                locals.get_mut(ctx).name = Some(String::from("ctx"));
                builder.local_set(ctx);
            } else {
                let func = maps.funcs.get(&i.func).unwrap();
                builder.local_get(ctx);
                builder.call(*func);
            }
        } else if instr.is_return_call() {
            let i = instr.unwrap_return_call();
            let func = maps.funcs.get(&i.func).unwrap();
            builder.local_get(ctx);
            builder.return_call(*func);
        } else if instr.is_call_indirect() {
            let i = instr.unwrap_call_indirect();
            builder.local_get(ctx);
            builder.unreachable();
        } else if instr.is_return_call_indirect() {
            let i = instr.unwrap_return_call_indirect();
            builder.local_get(ctx);
            builder.unreachable();
        // =================================================
        } else {
            builder.instr(instr.clone());
        }
    }
    stack.pop().unwrap().1
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = args().nth(1) else {
        println!("missing input file");
        return Ok(());
    };
    let wasm_bytes = std::fs::read(path)?;
    let module = walrus::ModuleConfig::new().parse(&wasm_bytes)?;
    let config = ModuleConfig::new();
    let mut new_module = Module::with_config(config);

    let ctx = new_module.locals.add(ValType::I64);
    new_module.locals.get_mut(ctx).name = Some(String::from("ctx"));

    let locals_map = module
        .locals
        .iter()
        .cloned()
        .map(|local| {
            let new_local = new_module.locals.add(local.ty());
            new_module.locals.get_mut(new_local).name = local.name.clone();
            (local.id(), new_local)
        })
        .collect::<HashMap<_, _>>();

    let mut ctx_set = None;
    let mut ctx_get = None;
    let mut funcs_map = HashMap::new();
    for old_func in module.functions() {
        let old_ty = module.types.get(old_func.ty());
        let mut new_params = old_ty.params().to_vec();
        new_params.push(ValType::I64);
        let new_results = old_ty.results().to_vec();
        let mut fb = FunctionBuilder::new(&mut new_module.types, &new_params, &new_results);
        match &old_func.kind {
            FunctionKind::Local(func) => {
                old_func
                    .name
                    .as_deref()
                    .map(|name| fb.name(name.to_string()));
                let args = func
                    .args
                    .iter()
                    .map(|arg| *locals_map.get(arg).unwrap())
                    .chain([ctx])
                    .collect::<Vec<_>>();
                let new_func = fb.finish(args, &mut new_module.funcs);
                funcs_map.insert(old_func.id(), new_func);
            }
            FunctionKind::Import(i) => {
                let import = module.imports.get(i.import);
                if import.name == "__ctx_get" {
                    ctx_get = Some(old_func.id());
                    continue;
                } else if import.name == "__ctx_set" {
                    ctx_set = Some(old_func.id());
                    continue;
                }
                let new_ty = new_module.types.add(old_ty.params(), old_ty.results());
                let new_import = new_module.add_import_func(&import.module, &import.name, new_ty);

                fb.name(format!("import_shim::{}", import.name));
                let mut body = fb.func_body();
                let args = old_ty
                    .params()
                    .iter()
                    .cloned()
                    .map(|ty| {
                        let arg = new_module.locals.add(ty);
                        body.local_get(arg);
                        arg
                    })
                    .chain([ctx])
                    .collect::<Vec<_>>();
                body.call(new_import.0);
                let import_shim = fb.finish(args, &mut new_module.funcs);
                funcs_map.insert(old_func.id(), import_shim);
            }
            FunctionKind::Uninitialized(_) => unreachable!(),
        }
    }

    let maps = Maps {
        funcs: funcs_map,
        locals: locals_map,
        ctx_get: ctx_get.expect("missing ctx_get"),
        ctx_set: ctx_set.expect("missing ctx_set"),
    };
    for old_func in module.functions() {
        if let FunctionKind::Local(func) = &old_func.kind {
            let new_id = maps.funcs.get(&old_func.id()).unwrap();
            let mut body = new_module
                .funcs
                .get_mut(*new_id)
                .kind
                .unwrap_local_mut()
                .builder_mut()
                .func_body();
            let mut stack = Vec::new();
            copy_seq(
                func,
                ctx,
                &mut stack,
                &maps,
                func.block(func.entry_block()),
                &mut body,
                &mut new_module.locals,
            );
        }
    }

    for e in module.exports.iter() {
        if let ExportItem::Function(f) = e.item {
            let ty = module.types.get(module.funcs.get(f).ty());
            let export_shim = *maps.funcs.get(&f).unwrap();
            let mut fb = FunctionBuilder::new(&mut new_module.types, ty.params(), ty.results());
            fb.name(format!("export_shim::{}", e.name));
            let mut body = fb.func_body();
            let args = ty
                .params()
                .iter()
                .cloned()
                .map(|ty| {
                    let arg = new_module.locals.add(ty);
                    body.local_get(arg);
                    arg
                })
                .chain([ctx])
                .collect::<Vec<_>>();
            body.i64_const(0);
            body.call(export_shim);
            let new_export = fb.finish(args, &mut new_module.funcs);
            new_module.exports.add(&e.name, new_export);
        } else {
            new_module.exports.add(&e.name, e.item);
        }
    }

    new_module.customs = module.customs;
    new_module.data = module.data;
    new_module.debug = module.debug;
    new_module.elements = module.elements;
    new_module.globals = module.globals;
    new_module.memories = module.memories;
    new_module.producers = module.producers;
    new_module.start = module.start.map(|func| *maps.funcs.get(&func).unwrap());
    // TODO: update tables
    new_module.tables = module.tables;

    let mut output = File::create("output.wasm")?;
    output.write_all(&new_module.emit_wasm())?;

    Ok(())
}
