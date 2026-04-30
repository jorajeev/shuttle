//! Decoder for the Shuttle scheduler step log format.
//!
//! Usage:
//!   cargo run --features metrics --bin sched_log_decode -- <file.sched.zst> [options]
//!
//! Options:
//!   --stats           Print per-run statistics (default when no other option given)
//!   --run <N>         Dump every step of run N (tab-separated: step, chosen, runnable_count, ids...)
//!   --run <N> --csv   Same but comma-separated

use shuttle::scheduler_log::{FILE_VERSION, MAGIC};
use std::collections::BTreeSet;
use std::env;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::PathBuf;

const TAG_RUN_START: u8 = 0x01;
const TAG_STEP: u8 = 0x02;
const TAG_RUN_END: u8 = 0x03;

fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

#[derive(Debug)]
struct RunStats {
    run_index: u64,
    seed: u64,
    num_steps: u64,
    max_runnable: usize,
}

enum Mode {
    Stats,
    DumpRun { run: u64, csv: bool },
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: sched_log_decode <file> [--stats | --run N [--csv]]");
        std::process::exit(1);
    }

    let path = PathBuf::from(&args[1]);
    let mode = parse_mode(&args[2..]);

    let file = File::open(&path)?;
    let decoder = zstd::Decoder::new(BufReader::new(file))?;
    let mut r = decoder;

    // Read and verify file header
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        eprintln!("error: not a Shuttle scheduler log file (bad magic)");
        std::process::exit(1);
    }
    let version = read_u8(&mut r)?;
    let _pad = read_u8(&mut r)?;
    if version != FILE_VERSION {
        eprintln!("error: unsupported file version {version} (expected {FILE_VERSION})");
        std::process::exit(1);
    }

    match mode {
        Mode::Stats => print_stats(&mut r),
        Mode::DumpRun { run, csv } => dump_run(&mut r, run, csv),
    }
}

fn parse_mode(args: &[String]) -> Mode {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--run" => {
                let run: u64 = args.get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| { eprintln!("--run requires a run number"); std::process::exit(1); });
                let csv = args.get(i + 2).map(|s| s == "--csv").unwrap_or(false);
                return Mode::DumpRun { run, csv };
            }
            _ => {}
        }
        i += 1;
    }
    Mode::Stats
}

fn print_stats(r: &mut impl Read) -> io::Result<()> {
    let mut stats: Vec<RunStats> = Vec::new();
    let mut runnable_set = BTreeSet::<u32>::new();
    let mut current_run: Option<(u64, u64)> = None; // (run_index, seed)
    let mut step_count = 0u64;
    let mut max_runnable = 0usize;

    loop {
        let tag = match read_u8(r) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        match tag {
            TAG_RUN_START => {
                let run_index = read_u64(r)?;
                let seed = read_u64(r)?;
                runnable_set.clear();
                step_count = 0;
                max_runnable = 0;
                current_run = Some((run_index, seed));
            }
            TAG_STEP => {
                let chosen = read_u32(r)?;
                let n_add = read_u8(r)? as usize;
                let n_rem = read_u8(r)? as usize;
                for _ in 0..n_add {
                    let id = read_u32(r)?;
                    runnable_set.insert(id);
                }
                for _ in 0..n_rem {
                    let id = read_u32(r)?;
                    runnable_set.remove(&id);
                }
                let _ = chosen;
                if runnable_set.len() > max_runnable {
                    max_runnable = runnable_set.len();
                }
                step_count += 1;
            }
            TAG_RUN_END => {
                let run_index = read_u64(r)?;
                let num_steps = read_u64(r)?;
                if let Some((idx, seed)) = current_run.take() {
                    debug_assert_eq!(idx, run_index);
                    debug_assert_eq!(step_count, num_steps);
                    stats.push(RunStats { run_index: idx, seed, num_steps, max_runnable });
                }
            }
            other => {
                eprintln!("error: unknown tag byte 0x{other:02x}");
                std::process::exit(1);
            }
        }
    }

    println!("{:<8}  {:<20}  {:<12}  {}", "run", "seed", "steps", "max_runnable");
    println!("{}", "-".repeat(58));
    for s in &stats {
        println!("{:<8}  {:<20}  {:<12}  {}", s.run_index, s.seed, s.num_steps, s.max_runnable);
    }
    println!();
    println!("total runs:  {}", stats.len());
    let total_steps: u64 = stats.iter().map(|s| s.num_steps).sum();
    println!("total steps: {}", total_steps);
    Ok(())
}

fn dump_run(r: &mut impl Read, target_run: u64, csv: bool) -> io::Result<()> {
    let sep = if csv { "," } else { "\t" };
    println!("step{sep}chosen{sep}runnable_count{sep}runnable_ids");

    let mut runnable_set = BTreeSet::<u32>::new();
    let mut in_target = false;
    let mut step = 0u64;

    loop {
        let tag = match read_u8(r) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        match tag {
            TAG_RUN_START => {
                let run_index = read_u64(r)?;
                let _seed = read_u64(r)?;
                runnable_set.clear();
                step = 0;
                in_target = run_index == target_run;
            }
            TAG_STEP => {
                let chosen = read_u32(r)?;
                let n_add = read_u8(r)? as usize;
                let n_rem = read_u8(r)? as usize;
                let mut adds = Vec::with_capacity(n_add);
                let mut rems = Vec::with_capacity(n_rem);
                for _ in 0..n_add {
                    adds.push(read_u32(r)?);
                }
                for _ in 0..n_rem {
                    rems.push(read_u32(r)?);
                }
                if in_target {
                    for id in adds {
                        runnable_set.insert(id);
                    }
                    for id in rems {
                        runnable_set.remove(&id);
                    }
                    let ids: Vec<String> = runnable_set.iter().map(|id| id.to_string()).collect();
                    println!("{step}{sep}{chosen}{sep}{}{sep}{}", runnable_set.len(), ids.join(sep));
                    step += 1;
                }
            }
            TAG_RUN_END => {
                let _run_index = read_u64(r)?;
                let _num_steps = read_u64(r)?;
                if in_target {
                    break;
                }
            }
            other => {
                eprintln!("error: unknown tag byte 0x{other:02x}");
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
