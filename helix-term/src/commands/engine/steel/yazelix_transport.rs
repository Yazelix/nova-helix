use serde_json::{json, Value};
use std::sync::mpsc::sync_channel;
use steel::{
    rvals::{AsRefSteelVal, Custom, IntoSteelVal, SteelString},
    steel_vm::{builtin::BuiltInModule, register_fn::RegisterFn},
    RootedSteelVal, SteelVal,
};

use crate::{
    compositor, job,
    remote::{BridgeServer, HandlerReceiver, HandlerRequest},
};

impl Custom for BridgeServer {}

fn start(auth_token: SteelString, handler: SteelVal) -> steel::rvals::Result<SteelVal> {
    if auth_token.is_empty() {
        steel::stop!(ContractViolation => "transport authentication token must be non-empty");
    }
    if !matches!(handler, SteelVal::Closure(_)) {
        steel::stop!(TypeMismatch => "transport handler must be a function");
    }

    let handler = handler.as_rooted();
    let generation = super::load_generation();
    BridgeServer::start(auth_token.as_str().to_owned(), move |request| {
        dispatch_to_steel(handler.value().as_rooted(), generation, request)
    })?
    .into_steelval()
}

fn local_addr(server: SteelVal) -> steel::rvals::Result<String> {
    Ok(BridgeServer::as_ref(&server)?.addr.to_string())
}

fn stop(server: SteelVal) -> steel::rvals::Result<()> {
    BridgeServer::as_ref(&server)?.stop();
    Ok(())
}

fn dispatch_to_steel(
    handler: RootedSteelVal,
    generation: usize,
    request: HandlerRequest,
) -> HandlerReceiver {
    let (sender, receiver) = sync_channel(1);
    job::dispatch_blocking_jobs(move |editor, _compositor, jobs| {
        if !super::is_current_generation(generation) {
            let _ = sender.send(Err("Steel engine was reloaded".into()));
            return;
        }
        let argument = match SteelVal::try_from(json!(request)) {
            Ok(argument) => argument,
            Err(error) => {
                let _ = sender.send(Err(error.to_string()));
                return;
            }
        };
        let mut compositor_context = compositor::Context {
            editor,
            jobs,
            scroll: None,
        };
        let mut context = super::with_context_guard(&mut compositor_context);
        let function = handler.value().clone();
        let result = super::enter_engine(|engine| {
            super::call_with_context_and_args(engine, &mut context, function, &mut [argument])
        })
        .and_then(Value::try_from)
        .map_err(|error| error.to_string());
        let _ = sender.send(result);
    });
    receiver
}

pub(super) fn module() -> BuiltInModule {
    let mut module = BuiltInModule::new("yazelix/transport");
    module
        .register_fn("transport-start", start)
        .register_fn("transport-local-addr", local_addr)
        .register_fn("transport-stop!", stop);
    module
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, TcpStream};
    #[test]
    fn production_engine_exposes_the_transport_lifecycle() {
        let mut engine =
            super::super::configure_engine_impl(steel::steel_vm::engine::Engine::new());
        let results = engine
            .compile_and_run_raw_program(
                r#"
                (require-builtin yazelix/transport)
                (define stopped-server (transport-start "secret" (lambda (request) request)))
                (assert! (string? (transport-local-addr stopped-server)))
                (transport-stop! stopped-server)
                (define owned-server (transport-start "secret" (lambda (request) request)))
                (transport-local-addr owned-server)
                "#,
            )
            .unwrap();
        let addr: SocketAddr = match results.last() {
            Some(steel::SteelVal::StringV(addr)) => addr.as_str().parse().unwrap(),
            value => panic!("expected transport address, got {value:?}"),
        };

        drop(results);
        drop(engine);
        assert!(TcpStream::connect(addr).is_err());
    }
}
