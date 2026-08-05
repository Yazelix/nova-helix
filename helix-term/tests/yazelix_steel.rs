#![cfg(feature = "steel")]
use steel::{
    rvals::SteelString,
    steel_vm::{builtin::BuiltInModule, engine::Engine, register_fn::RegisterFn},
    SteelVal,
};

fn transport_start(token: SteelString, handler: SteelVal) -> steel::rvals::Result<SteelVal> {
    if token.as_str() != "caller-token" {
        steel::stop!(ContractViolation => "test transport received the wrong token");
    }
    Ok(handler)
}

#[test]
fn bridge_composes_transport_validation_and_editor_actions() {
    let mut engine = Engine::new();
    let mut transport = BuiltInModule::new("yazelix/transport");
    transport.register_fn("transport-start", transport_start);
    engine.register_module(transport);

    engine.register_steel_module(
        "helix/commands.scm".into(),
        r#"
        (provide change-current-directory open calls)
        (define action-log '())
        (define (record name args) (set! action-log (cons (cons name args) action-log)))
        (define (change-current-directory . args) (record "cd" args))
        (define (open . args) (record "open" args))
        (define (calls) (reverse action-log))
        "#
        .into(),
    );

    engine
        .compile_and_run_raw_program_with_path(
            include_str!("yazelix_steel.scm"),
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/yazelix_steel.scm").into(),
        )
        .unwrap();
}
