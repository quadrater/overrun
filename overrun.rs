use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Parser;
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::geteuid;
use tempfile::TempDir;

use lms::core::synchronize;
use lms::parse::Flag;

#[derive(Parser, Debug)]
#[command(name = "overrun", version)]
struct Args {
    #[arg(value_name="DIR", default_value=".")]
    dir: String,
    #[arg(last = true, required=true)]
    cmd: Vec<String>,
}

fn mnt_private() -> Result<(), Box<dyn std::error::Error>> {
    unsafe { mount::<&str, _, &str, &str>(None, Path::new("/"), None, MsFlags::MS_REC | MsFlags::MS_PRIVATE, None)?; }
    Ok(())
}

fn bind(from: &Path, to: &Path) -> Result<(), Box<dyn std::error::Error>> {
    unsafe { mount(Some(from), to, None::<&str>, MsFlags::MS_BIND | MsFlags::MS_REC, None::<&str>)?; }
    Ok(())
}

fn ovl(target: &Path, lower: &Path, upper: &Path, work: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let opts = format!("lowerdir={},upperdir={},workdir={}", lower.display(), upper.display(), work.display());
    unsafe { mount(Some("overlay"), target, Some("overlay"), MsFlags::empty(), Some(&opts))?; }
    Ok(())
}

fn lazy_umount(p: &Path) { let _ = umount2(p, MntFlags::MNT_DETACH); }

fn main() -> ExitCode {
    let args = Args::parse();

    if geteuid().as_raw() != 0 {
        let mut cmd = Command::new("sudo");
        cmd.arg(env::current_exe().unwrap());
        cmd.arg(&args.dir);
        cmd.arg("--");
        cmd.args(&args.cmd);
        let st = cmd.status().expect("sudo failed");
        return ExitCode::from(st.code().unwrap_or(1) as u8);
    }

    if let Err(e) = unshare(CloneFlags::CLONE_NEWNS) { eprintln!("unshare failed: {e}"); return ExitCode::from(1); }
    if let Err(e) = mnt_private() { eprintln!("propagation failed: {e}"); return ExitCode::from(1); }

    let dir_abs = match fs::canonicalize(&args.dir) { Ok(p) => p, Err(e) => { eprintln!("canonicalize failed: {e}"); return ExitCode::from(1); } };

    let tmp = match TempDir::new() { Ok(t) => t, Err(e) => { eprintln!("tempdir failed: {e}"); return ExitCode::from(1); } };
    let base = tmp.path().to_path_buf();
    let orig = base.join("orig");
    let upper = base.join("upper");
    let work  = base.join("work");
    for d in [&orig, &upper, &work] { if let Err(e) = fs::create_dir_all(d) { eprintln!("mkdir {:?} failed: {e}", d); return ExitCode::from(1); } }

    if let Err(e) = bind(&dir_abs, &orig) { eprintln!("bind mount failed: {e}"); return ExitCode::from(1); }
    if let Err(e) = ovl(&dir_abs, &orig, &upper, &work) { eprintln!("overlay mount failed: {e}"); lazy_umount(&orig); return ExitCode::from(1); }

    let status = Command::new(&args.cmd[0]).args(&args.cmd[1..]).current_dir(&dir_abs).status();
    let code = status.map(|s| s.code().unwrap_or(1)).unwrap_or(127);

    if let Err(e) = synchronize(dir_abs.to_str().unwrap(), orig.to_str().unwrap(), Flag::empty()) { eprintln!("merge failed: {e}"); }

    lazy_umount(&dir_abs);
    lazy_umount(&orig);

    ExitCode::from(if (0..=255).contains(&code) { code as u8 } else { 1 })
}