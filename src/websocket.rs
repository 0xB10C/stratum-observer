use crate::types::JobUpdate;
use crate::types::JobUpdateJson;
use async_broadcast::broadcast;
use async_channel::Receiver;
use async_std::sync::Arc;
use async_std::sync::RwLock;
use async_std::task;
use log::{debug, error, info, warn};
use std::collections::BTreeMap;
use std::net::{TcpListener, TcpStream};
use tungstenite::{accept, WebSocket};

/// Serializes a job and sends it into the websocket. Returns false if the job
/// could not be sent, which usually means the connection is gone.
fn send_job(websocket: &mut WebSocket<TcpStream>, job: &JobUpdate) -> bool {
    let msg = match serde_json::to_string::<JobUpdateJson>(&job.clone().into()) {
        Ok(msg) => msg,
        Err(e) => {
            warn!("Could not serialize JobUpdateJson to JSON: {}", e);
            // A job we can't serialize is skipped, the connection is still fine.
            return true;
        }
    };
    if let Err(e) = websocket.send(tungstenite::Message::Text(msg)) {
        debug!(
            "Could not send '{}' job update to websocket: {}. Connection probably closed.",
            job.pool.name, e
        );
        return false;
    }
    true
}

/// Binds a websocket server to the given address and sends all jobs it
/// receives to the connected websocket clients.
pub async fn websocket_sender_task(receiver: Receiver<JobUpdate<'static>>, ws_addr: &str) {
    info!("Starting websocket server on {}", ws_addr);
    let listener = match TcpListener::bind(ws_addr) {
        Ok(l) => l,
        Err(e) => {
            error!("Could not start websocket server on {}: {}", ws_addr, e);
            return;
        }
    };
    websocket_sender_task_with_listener(receiver, listener).await
}

/// Sends all jobs it receives to the clients connecting to the already bound
/// listener. Tests use this to listen on a port picked by the operating
/// system.
pub async fn websocket_sender_task_with_listener(
    receiver: Receiver<JobUpdate<'static>>,
    listener: TcpListener,
) {
    // a broadcast channel is used to fan-out the jobs to all websocket
    // subscribers. One 'inactive' receiver is just used for cloning new
    // broadcast receivers. The sender overflows to make sure we allways
    // empty the websocket_receiver channel, even when no active websocket
    // connections are avaliable.
    let (mut sender, broadcast_receiver) = broadcast(10);
    let inactive_broadcast_receiver = broadcast_receiver.deactivate();
    sender.set_overflow(true);
    let recent_jobs: BTreeMap<String, JobUpdate> = BTreeMap::new();
    let recent_jobs_rwl_arc = Arc::new(RwLock::new(recent_jobs));

    let recent_jobs_write = recent_jobs_rwl_arc.clone();
    task::spawn(async move {
        loop {
            let job = receiver.recv().await.unwrap();
            {
                // store the most recent job in a map to be able to send
                // it to newly connecting clients
                let mut recent_jobs_ = recent_jobs_write.write().await;
                recent_jobs_.insert(job.pool.name.clone(), job.clone());
            }
            if sender.receiver_count() > 0 {
                sender.broadcast(job.clone()).await.unwrap();
                debug!(
                    "broadcast new job by '{}' to {} websocket thread(s)",
                    job.pool.name,
                    sender.receiver_count()
                );
            }
        }
    });

    // Waiting for a new connection blocks, so it happens on a thread of its
    // own. Otherwise it would block one of the threads the async tasks run on.
    let (stream_sender, stream_receiver) = async_channel::unbounded();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if stream_sender.send_blocking(stream).is_err() {
                        debug!("Nobody handles new websocket connections anymore, stop listening");
                        return;
                    }
                }
                Err(e) => {
                    warn!("Failed to accept incomming TCP connection: {}", e);
                }
            }
        }
    });

    while let Ok(stream) = stream_receiver.recv().await {
        let mut r = inactive_broadcast_receiver.clone().activate();
        let recent_jobs_read = recent_jobs_rwl_arc.clone();
        task::spawn(async move {
            let mut websocket = match accept(stream) {
                Ok(websocket) => websocket,
                Err(e) => {
                    warn!("Failed to open websocket on incoming connection: {}", e);
                    return;
                }
            };
            debug!(
                "Accepted new websocket connection: connections={}",
                r.receiver_count()
            );

            // send recent jobs when websocket is opened
            let mut connected = true;
            {
                let recent_jobs_ = recent_jobs_read.read().await;
                for job in recent_jobs_.values() {
                    if !send_job(&mut websocket, job) {
                        connected = false;
                        break;
                    }
                }
            }

            // continuesly send new jobs
            while connected {
                let job = match r.recv().await {
                    Ok(j) => j,
                    Err(e) => {
                        warn!("Could not receive broadcast-job for websocket: {}", e);
                        break;
                    }
                };
                if !send_job(&mut websocket, &job) {
                    break;
                }
            }

            // Try our best to close and flush the websocket. If we can't,
            // we can't..
            let _ = websocket.close(None);
            let _ = websocket.flush();
        });
    }
    info!("Stopped accepting websocket connections");
}
