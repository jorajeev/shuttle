use shuttle::check_dfs;
use shuttle::future::{self, batch_semaphore::*};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use test_log::test;

#[test]
fn basic_semaphore() {
    check_dfs(
        || {
            let s = BatchSemaphore::new(3);

            future::spawn(async move {
                s.acquire(2).await.unwrap();
                s.acquire(1).await.unwrap();
                let r = s.try_acquire(1);
                assert_eq!(r, Err(TryAcquireError::NoPermits));
                s.release(1);
                s.acquire(1).await.unwrap();
            });
        },
        None,
    );
}

async fn semtest(num_permits: usize, counts: Vec<usize>, states: &Arc<Mutex<HashSet<(usize, usize)>>>) {
    let s = Arc::new(BatchSemaphore::new(num_permits));
    let r = Arc::new(AtomicUsize::new(0));
    let mut handles = vec![];
    for (i, &c) in counts.iter().enumerate() {
        let s = s.clone();
        let r = r.clone();
        let states = states.clone();
        let val = 1usize << i;
        handles.push(future::spawn(async move {
            s.acquire(c).await.unwrap();
            let v = r.fetch_add(val, Ordering::SeqCst);
            future::yield_now().await;
            let _ = r.fetch_sub(val, Ordering::SeqCst);
            states.lock().unwrap().insert((i, v));
            s.release(c);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
}

#[test]
fn semtest_1() {
    let states = Arc::new(Mutex::new(HashSet::new()));
    let states2 = states.clone();
    check_dfs(
        move || {
            let states2 = states2.clone();
            future::block_on(async move {
                semtest(5, vec![3, 3, 3], &states2).await;
            });
        },
        None,
    );

    let states = Arc::try_unwrap(states).unwrap().into_inner().unwrap();
    assert_eq!(states, HashSet::from([(0, 0), (1, 0), (2, 0)]));
}

#[test]
fn semtest_2() {
    let states = Arc::new(Mutex::new(HashSet::new()));
    let states2 = states.clone();
    check_dfs(
        move || {
            let states2 = states2.clone();
            future::block_on(async move {
                semtest(5, vec![3, 3, 2], &states2).await;
            });
        },
        None,
    );

    let states = Arc::try_unwrap(states).unwrap().into_inner().unwrap();
    assert_eq!(
        states,
        HashSet::from([(0, 0), (1, 0), (2, 0), (0, 4), (1, 4), (2, 1), (2, 2)])
    );
}
