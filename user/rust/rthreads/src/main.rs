//! Rust の標準ライブラリのスレッドを一通り使う(結果は Linux と比べる)。

use std::sync::{Arc, Mutex, mpsc};
use std::thread;

fn main() {
    // Mutex:4 つのスレッドで 1 つのカウンタを増やす
    let counter = Arc::new(Mutex::new(0u64));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let c = Arc::clone(&counter);
            thread::spawn(move || {
                for _ in 0..10_000 {
                    *c.lock().unwrap() += 1;
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    println!("mutex: {}", *counter.lock().unwrap());

    // チャンネル:3 つの送り手から 1 つの受け手へ
    let (tx, rx) = mpsc::channel();
    for id in 0..3u64 {
        let tx = tx.clone();
        thread::spawn(move || {
            for i in 0..100 {
                tx.send(id * 1000 + i).unwrap();
            }
        });
    }
    drop(tx);
    println!("channel: {}", rx.iter().sum::<u64>());

    // scope:借りたデータを分けて、スレッドごとに書き換える
    let mut data = vec![1u64; 1000];
    thread::scope(|s| {
        for chunk in data.chunks_mut(250) {
            s.spawn(move || chunk.iter_mut().for_each(|x| *x *= 2));
        }
    });
    println!("scope: {}", data.iter().sum::<u64>());

    // 名前と大きなスタック(uv は main をこうして別のスレッドで動かす)
    let name = thread::Builder::new()
        .name("worker".into())
        .stack_size(4 << 20)
        .spawn(|| thread::current().name().map(String::from))
        .unwrap()
        .join()
        .unwrap();
    println!("builder: {name:?}");
    let cpus = thread::available_parallelism().map_or(0, |n| n.get());
    println!("available_parallelism >= 1: {}", cpus >= 1);
}
