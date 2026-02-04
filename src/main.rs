use std::{
    thread,
    time::{Duration, Instant},
};

use clap::Parser;
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError, TrySendError};
use tokio::runtime::Builder;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Parser, Debug)]
#[command(
    name = "xb-tokio-try-send",
    about = "Tokio tasks try_send into a crossbeam channel; receiver thread try_recv."
)]
struct Args {
    /// How long to run the workload (seconds)
    #[arg(long)]
    duration: u64,

    /// Number of tokio sender tasks
    #[arg(long, default_value_t = 8)]
    tasks: usize,

    /// Channel capacity
    #[arg(long, default_value_t = 50_000)]
    capacity: usize,

    /// Tokio worker threads
    #[arg(long, default_value_t = 8)]
    worker_threads: usize,
}

pub enum Error {
    Disconnected,
}

pub fn recv_packet_batches(recvr: &Receiver<(usize, u64)>) -> Result<(usize, Duration), Error> {
    const MAX_RECV_ATTEMPTS: usize = 1_000;
    let recv_start = Instant::now();

    let mut num_packets = 0;
    let mut num_attempts = 0;

    while num_attempts < MAX_RECV_ATTEMPTS {
        loop {
            match recvr.try_recv() {
                Ok((_task_id, _counter)) => {
                    num_packets += 1;
                }
                Err(TryRecvError::Empty) => {
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(Error::Disconnected);
                }
            }
        }
        if num_packets > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
        num_attempts += 1;
    }
    let recv_duration = recv_start.elapsed();
    Ok((num_packets, recv_duration))
}

fn receiver_thread(rx: Receiver<(usize, u64)>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let start = Instant::now();
        let mut total: usize = 0;

        loop {
            match recv_packet_batches(&rx) {
                Ok((num_packets, _recv_duration)) => {
                    total += num_packets;
                    if total > 1_000_000 {
                        let elapsed = start.elapsed().as_millis();
                        println!("Elapsed: {elapsed}ms, received {total} ",);
                        total = 0;
                    }
                }
                Err(_) => {
                    println!("Receiver: channel disconnected, exiting");
                    return;
                }
            }
        }
    })
}

async fn sender_task(task_id: usize, tx: Sender<(usize, u64)>, cancel: CancellationToken) {
    let mut counter: u64 = 0;
    // Desync task wakeups (phase offset) and reduce burstiness (smaller batches, faster ticks).
    const BATCH_SIZE: usize = 32;
    const TICK_US: u64 = 250;
    const PHASE_US: u64 = 5_000;
    const YIELD_EVERY: usize = 8;

    let phase_us = (task_id as u64 * 137) % PHASE_US;
    let start = tokio::time::Instant::now() + Duration::from_micros(phase_us);
    let mut ticker = tokio::time::interval_at(start, Duration::from_micros(TICK_US));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                // send 128 messages every 1ms. so each task aims for 128k msgs/sec
                for i in 0..BATCH_SIZE {
                    counter += 1;
                    match tx.try_send((task_id, counter)) {
                        Ok(()) => {
                        }
                        Err(TrySendError::Full(_)) => {
                            println!("Task {task_id}: channel full, yielding");
                            tokio::task::yield_now().await;
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            cancel.cancel();
                            break;
                        }
                    }
                    if i % YIELD_EVERY == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
    }
}

fn main() {
    let args = Args::parse();

    let (tx, rx) = bounded::<(usize, u64)>(args.capacity);

    let recv_handle = receiver_thread(rx);

    let rt = Builder::new_multi_thread()
        .worker_threads(args.worker_threads)
        .enable_time()
        .build()
        .expect("tokio runtime");

    rt.block_on(async move {
        let cancel = CancellationToken::new();

        let tasks = TaskTracker::new();
        for task_id in 0..args.tasks {
            let tx_cloned = tx.clone();
            let token = cancel.clone();
            tasks.spawn(sender_task(task_id, tx_cloned, token));
        }
        tasks.close();

        drop(tx);

        tokio::time::sleep(Duration::from_secs(args.duration)).await;

        // Cancel all tasks
        cancel.cancel();
        tasks.wait().await;

        // After tasks complete, all Sender clones are dropped => receiver sees Disconnected.
    });

    let _ = recv_handle.join();
}
