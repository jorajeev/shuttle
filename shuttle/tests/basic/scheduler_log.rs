#[cfg(feature = "metrics")]
mod tests {
    use shuttle::scheduler::RandomScheduler;
    use shuttle::scheduler_log::{FILE_VERSION, MAGIC};
    use shuttle::{scheduler_log::SchedulerLogConfig, Config, Runner};
    use std::collections::BTreeSet;
    use std::io::{BufReader, Read};
    use tempfile::NamedTempFile;

    const TAG_RUN_START: u8 = 0x01;
    const TAG_STEP: u8 = 0x02;
    const TAG_RUN_END: u8 = 0x03;

    fn read_u8(r: &mut impl Read) -> u8 {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).unwrap();
        b[0]
    }
    fn read_u32(r: &mut impl Read) -> u32 {
        let mut b = [0u8; 4];
        r.read_exact(&mut b).unwrap();
        u32::from_le_bytes(b)
    }
    fn read_u64(r: &mut impl Read) -> u64 {
        let mut b = [0u8; 8];
        r.read_exact(&mut b).unwrap();
        u64::from_le_bytes(b)
    }

    struct DecodedRun {
        run_index: u64,
        #[allow(dead_code)]
        seed: u64,
        steps: Vec<(u32, BTreeSet<u32>)>, // (chosen, runnable_set)
    }

    fn decode_log(path: &std::path::Path) -> Vec<DecodedRun> {
        let file = std::fs::File::open(path).unwrap();
        let decoder = zstd::Decoder::new(BufReader::new(file)).unwrap();
        let mut r = decoder;

        let mut magic = [0u8; 8];
        r.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, MAGIC);
        let version = read_u8(&mut r);
        assert_eq!(version, FILE_VERSION);
        let _pad = read_u8(&mut r);

        let mut runs = Vec::new();
        let mut current: Option<(u64, u64, BTreeSet<u32>, Vec<(u32, BTreeSet<u32>)>)> = None;

        loop {
            let tag = {
                let mut b = [0u8; 1];
                match r.read_exact(&mut b) {
                    Ok(()) => b[0],
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => panic!("read error: {e}"),
                }
            };
            match tag {
                TAG_RUN_START => {
                    let run_index = read_u64(&mut r);
                    let seed = read_u64(&mut r);
                    current = Some((run_index, seed, BTreeSet::new(), Vec::new()));
                }
                TAG_STEP => {
                    let chosen = read_u32(&mut r);
                    let n_add = read_u8(&mut r) as usize;
                    let n_rem = read_u8(&mut r) as usize;
                    let (_, _, ref mut runnable, ref mut steps) = *current.as_mut().unwrap();
                    for _ in 0..n_add { runnable.insert(read_u32(&mut r)); }
                    for _ in 0..n_rem { runnable.remove(&read_u32(&mut r)); }
                    steps.push((chosen, runnable.clone()));
                }
                TAG_RUN_END => {
                    let _run_index = read_u64(&mut r);
                    let _num_steps = read_u64(&mut r);
                    let (run_index, seed, _, steps) = current.take().unwrap();
                    assert_eq!(_run_index, run_index);
                    assert_eq!(_num_steps as usize, steps.len());
                    runs.push(DecodedRun { run_index, seed, steps });
                }
                other => panic!("unknown tag 0x{other:02x}"),
            }
        }
        runs
    }

    #[test]
    fn log_has_correct_run_count() {
        let f = NamedTempFile::new().unwrap();
        let config = Config::new().with_scheduler_log(SchedulerLogConfig::new(f.path()));
        let iters = Runner::new(RandomScheduler::new(10), config).run(|| {
            let _ = shuttle::thread::spawn(|| {});
        });
        let runs = decode_log(f.path());
        assert_eq!(runs.len(), iters);
    }

    #[test]
    fn chosen_task_is_always_in_runnable_set() {
        let f = NamedTempFile::new().unwrap();
        let config = Config::new().with_scheduler_log(SchedulerLogConfig::new(f.path()));
        Runner::new(RandomScheduler::new(20), config).run(|| {
            let _ = shuttle::thread::spawn(|| {});
        });
        let runs = decode_log(f.path());
        for run in &runs {
            for (step_idx, (chosen, runnable)) in run.steps.iter().enumerate() {
                assert!(
                    runnable.contains(chosen),
                    "run {} step {}: chosen task {} not in runnable set {:?}",
                    run.run_index, step_idx, chosen, runnable
                );
            }
        }
    }

    #[test]
    fn spawned_task_appears_in_runnable_set() {
        let f = NamedTempFile::new().unwrap();
        let config = Config::new().with_scheduler_log(SchedulerLogConfig::new(f.path()));
        Runner::new(RandomScheduler::new(20), config).run(|| {
            // Spawn a task (task ID 1) — it must appear as runnable at some step.
            let t = shuttle::thread::spawn(|| {});
            t.join().unwrap();
        });
        let runs = decode_log(f.path());
        // Task 1 must appear in the runnable set at least once across all runs
        let saw_task_1 = runs.iter().any(|r| r.steps.iter().any(|(_, s)| s.contains(&1)));
        assert!(saw_task_1, "task 1 never appeared in the runnable set");
    }
}
