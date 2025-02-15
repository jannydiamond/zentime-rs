use crate::config::Config;
use crate::ipc::{
    get_socket_name, ClientToServerMsg, InterProcessCommunication, ServerToClientMsg,
};
use crate::server::notification::dispatch_notification;
use crate::server::timer_output::TimerOutputAction;
use anyhow::Context;
use crossbeam::channel::{unbounded, Sender};
use interprocess::local_socket::tokio::OwnedWriteHalf;
use log::{error, info};
use tokio::task::{spawn_blocking, yield_now};
use zentime_rs_timer::pomodoro_timer::{PomodoroTimer, TimerKind};
use zentime_rs_timer::pomodoro_timer_action::PomodoroTimerAction;

use std::rc::Rc;
use std::sync::Arc;
use tokio::select;
use tokio::sync::{self, broadcast::Receiver as BroadcastReceiver};

use futures::io::BufReader;
use interprocess::local_socket::tokio::{LocalSocketListener, LocalSocketStream};

use std::time::Duration;
use tokio::fs::{metadata, remove_file};


use super::status::{server_status, ServerStatus};

/// Starts the server by opening the zentime socket and listening for incoming connections.
/// This will just quit if another zentime server process is already running.
///
/// NOTE:
/// This spawns a tokio runtime and should therefore not be run inside another tokio runtime.
#[tokio::main]
pub async fn start(config: Config) -> anyhow::Result<()> {
    let socket_name = get_socket_name();

    let socket_file_already_exists = metadata(socket_name).await.is_ok();

    if socket_file_already_exists && server_status() == ServerStatus::Running {
        info!("Server is already running. Terminating this process...");
        // Apparently a server is already running and we don't need to do anything
        return Ok(());
    }

    if socket_file_already_exists {
        info!("Socket file already exists - removing file");

        // We have a dangling socket file without an attached server process.
        // In that case we simply remove the file and start a new server process
        remove_file(socket_name)
            .await
            .context("Could not remove existing socket file")?
    };

    info!("Start listening for connections...");

    listen(config, socket_name)
        .await
        .context("Error while listening for connections")?;

    Ok(())
}

/// This starts a blocking tokio task which runs the actual synchronous timer logic, but
/// also listens for incoming client connections and spawns a new async task for each incoming
/// connection.
async fn listen(config: Config, socket_name: &str) -> anyhow::Result<()> {
    info!("Binding to socket...");
    let listener =
        LocalSocketListener::bind(socket_name).context("Could not bind to local socket")?;

    let (timer_input_sender, timer_input_receiver) = unbounded();
    let (timer_output_sender, _timer_output_receiver) = sync::broadcast::channel(24);

    let timer_output_sender = Arc::new(timer_output_sender.clone());
    // Arc clone to create a reference to our sender which can be consumed by the
    // timer thread. This is necessary because we need a reference to this sender later on
    // to continuously subscribe to it on incoming client connections
    let timer_out_tx = timer_output_sender.clone();

    spawn_blocking(move || {
        info!("Starting timer...");

        PomodoroTimer::new(
            config.timers,
            Rc::new(move |_, msg, kind| {
                let result = dispatch_notification(
                    config.clone().notifications,
                    msg,
                    kind == TimerKind::Interval
                );

                if let Err(error) = result {
                    error!("{}", error);
                }
            }),
            Rc::new(move |view_state| {
                // Update the view
                timer_out_tx.send(TimerOutputAction::Timer(view_state)).ok();

                // Handle app actions and hand them to the timer caller
                match timer_input_receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(action) => Some(action),
                    _ => Some(PomodoroTimerAction::None),
                }
            }),
        )
        .init()
    });

    // Set up our loop boilerplate that processes our incoming connections.
    loop {
        let connection = listener
            .accept()
            .await
            .context("There was an error with an incoming connection")?;

        let input_tx = timer_input_sender.clone();
        let output_rx = timer_output_sender.subscribe();

        // Spawn new parallel asynchronous tasks onto the Tokio runtime
        // and hand the connection over to them so that multiple clients
        // could be processed simultaneously in a lightweight fashion.
        tokio::spawn(async move {
            info!("New connection received.");
            if let Err(error) = handle_conn(connection, input_tx, output_rx).await {
                error!("Could not handle connection: {}", error);
            };
        });
    }
}

/// Describe the things we do when we've got a connection ready.
/// This will continously send the current timer state to the client and also listen for incoming
/// [ClientToServerMsg]s.
async fn handle_conn(
    conn: LocalSocketStream,
    timer_input_sender: Sender<PomodoroTimerAction>,
    mut timer_output_receiver: BroadcastReceiver<TimerOutputAction>,
) -> anyhow::Result<()> {
    // Split the connection into two halves to process
    // received and sent data concurrently.
    let (reader, mut writer) = conn.into_split();
    let mut reader = BufReader::new(reader);

    loop {
        select! {
            msg = InterProcessCommunication::recv_ipc_message::<ClientToServerMsg>(&mut reader) => {
                let msg = msg.context("Could not receive message from socket")?;
                if let CloseConnection::Yes = handle_client_to_server_msg(msg, &timer_input_sender)
                    .await
                    .context("Could not handle client to server message")? {
                        break;
                    };
            },
            value = timer_output_receiver.recv() => {
                let action = value.context("Could not receive output from timer")?;
                handle_timer_output_action(action, &mut writer).await.context("Couuld not handle timer output action")?;
            }
        }

        yield_now().await;
    }

    info!("Closing connection");
    Ok(())
}

enum CloseConnection {
    Yes,
    No,
}

async fn handle_client_to_server_msg(
    msg: ClientToServerMsg,
    timer_input_sender: &Sender<PomodoroTimerAction>,
) -> anyhow::Result<CloseConnection> {
    match msg {
        // Shutdown server
        ClientToServerMsg::Quit => {
            info!("\nClient told server to shutdown");

            info!("Cleaning up socket file");
            let socket_name = get_socket_name();
            remove_file(socket_name)
                .await
                .context("Could not remove existing socket file")?;

            info!("Shutting down...");
            std::process::exit(0);
        }

        ClientToServerMsg::Reset => {
            timer_input_sender
                .send(PomodoroTimerAction::ResetTimer)
                .context("Could not send ResetTimer to timer")?;
        }

        // Play/Pause the timer
        ClientToServerMsg::PlayPause => {
            timer_input_sender
                .send(PomodoroTimerAction::PlayPause)
                .context("Could not send Play/Pause to timer")?;
        }

        // Skip to next timer interval
        ClientToServerMsg::Skip => {
            timer_input_sender
                .send(PomodoroTimerAction::Skip)
                .context("Could not send Skip to timer")?;
        }

        // Try to postpone the current break (limited by pomodoro timer config and state)
        ClientToServerMsg::PostPone => {
            timer_input_sender
                .send(PomodoroTimerAction::PostponeBreak)
                .context("Could not send Skip to timer")?;
        }

        // Close connection, because client has detached
        ClientToServerMsg::Detach => {
            info!("Client detached.");
            return Ok(CloseConnection::Yes);
        }

        ClientToServerMsg::Sync => {
            info!("Client synced with server");
        }
    }

    Ok(CloseConnection::No)
}

async fn handle_timer_output_action(
    action: TimerOutputAction,
    writer: &mut OwnedWriteHalf,
) -> anyhow::Result<()> {
    let TimerOutputAction::Timer(state) = action;
    let msg = ServerToClientMsg::Timer(state);
    InterProcessCommunication::send_ipc_message(msg, writer)
        .await
        .context("Could not send IPC message from server to client")?;

    Ok(())
}
