#![cfg(feature = "steel")]
use steel::steel_vm::engine::Engine;

#[test]
fn bridge_actions_preserve_workspace_and_target_order() {
    let mut engine = Engine::new();
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
    engine.register_steel_module(
        "yazelix/bridge-actions.scm".into(),
        include_str!("../../yazelix/steel/bridge-actions.scm").into(),
    );

    engine
        .compile_and_run_raw_program(
            r#"
            (require "yazelix/bridge-actions.scm"
                     (only-in "helix/commands.scm" calls))
            (yzx-helix-open-files "/workspace" "/one" "/two")
            (yzx-helix-open-directory "/other-workspace" "/picker")
            (assert! (equal? (calls)
                             '(("cd" "/workspace")
                               ("open" "/one" "/two")
                               ("cd" "/other-workspace")
                               ("open" "/picker"))))
            "#,
        )
        .unwrap();
}
