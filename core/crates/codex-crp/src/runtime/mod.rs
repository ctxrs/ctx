mod commands;
mod io;
mod prompt_items;
mod server_requests;
mod session;
mod state;
mod status;
#[cfg(test)]
mod tests;
mod translate;

use crate::app_server::AppServerInbound;
use crate::RuntimeOptions;
use anyhow::Result;
use tokio::sync::mpsc;
use tracing::error;

use self::commands::handle_command;
use self::io::{read_commands, run_writer, CrpEventRouter, CrpWriter};
use self::state::{current_model_id, AppServerSessionState, TurnAliasState, TurnTracker};
use self::translate::{
    canonical_context_window_from_thread_usage, emit_turn_request_error,
    emit_unsupported_server_request_notice,
};

const DATA_PLANE_BUFFER_CAPACITY: usize = 256;

enum RuntimeCommand {
    Parsed(Box<crate::protocol::CrpCommand>),
    ParseError { message: String },
}

enum RuntimeInput {
    Command(Box<RuntimeCommand>),
    AppServer(AppServerInbound),
}

pub async fn run(options: RuntimeOptions) -> Result<()> {
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(read_commands(cmd_tx));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let (data_tx, data_rx) = mpsc::channel(DATA_PLANE_BUFFER_CAPACITY);
    let router = CrpEventRouter::new(control_tx, data_tx);

    let writer = CrpWriter::new();
    tokio::spawn(async move {
        if let Err(err) = run_writer(writer, control_rx, data_rx).await {
            error!(?err, "crp writer task failed");
        }
    });

    let mut session: Option<AppServerSessionState> = None;

    loop {
        match next_runtime_input(&mut session, &mut cmd_rx).await {
            Some(RuntimeInput::Command(command)) => {
                handle_command(*command, &mut session, &router, &options).await?;
            }
            Some(RuntimeInput::AppServer(event)) => {
                if let Some(session_state) = session.as_mut() {
                    server_requests::handle_app_server_event(session_state, event, &router).await;
                }
            }
            None => break,
        }
    }

    if let Some(session_state) = session.as_mut() {
        session_state.client.shutdown().await;
    }

    Ok(())
}

async fn next_runtime_input(
    session: &mut Option<AppServerSessionState>,
    cmd_rx: &mut mpsc::UnboundedReceiver<RuntimeCommand>,
) -> Option<RuntimeInput> {
    let Some(session_state) = session.as_mut() else {
        return cmd_rx
            .recv()
            .await
            .map(|command| RuntimeInput::Command(Box::new(command)));
    };
    tokio::select! {
        Some(cmd) = cmd_rx.recv() => Some(RuntimeInput::Command(Box::new(cmd))),
        maybe_event = session_state.client.next_inbound() => maybe_event.map(RuntimeInput::AppServer),
        else => None,
    }
}
