//! Windows: commands run in an AppContainer, the sandbox Windows uses for
//! Store apps and browser tabs. A process in one can open a file only if the
//! file's permissions name that container (or a capability it holds). Being
//! you isn't enough, and neither is "Everyone". So the connector grants:
//!
//! - the folder and the session's temp folder: full access, to this folder's
//!   own container (one per folder, so one folder's container can't open another),
//!   and a low integrity label: containers run at low integrity, and Windows
//!   doesn't let those write to anything labelled higher, whatever the
//!   permissions say (the same label Windows gives AppData\LocalLow);
//! - your toolchains (Rust, Node, Python... under your user folder): read and
//!   run, to a capability every container gets.
//!
//! System folders (Windows, Program Files) already let containers read them.
//! Nothing else under your user folder does: documents, SSH keys and other
//! projects stay out of reach. Each command also runs in a job, so stopping
//! it stops everything it started.
//!
//! The grants stay on the folder after the session (they only let that
//! folder's own container in), so a big folder is set up once, not every time.

use super::{Fence, Support};
use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, SetHandleInformation, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, SDDL_REVISION_1, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W};
use windows_sys::Win32::Security::Isolation::{CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName};
use windows_sys::Win32::Security::{
    DeriveCapabilitySidsFromName, EqualSid, FreeSid, GetAce, GetAclInformation, GetSecurityDescriptorControl, GetSecurityDescriptorSacl, LABEL_SECURITY_INFORMATION, AclSizeInformation, ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, INHERITED_ACE, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES, SE_DACL_PROTECTED, SID_AND_ATTRIBUTES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE};
use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, FILE_PERSISTENT_ACLS, SE_GROUP_ENABLED, SYSTEM_MANDATORY_LABEL_ACE_TYPE};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED, EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// Read, write, delete, everything: for the folder and the temp folder.
const FULL: u32 = 0x001F_01FF;
/// Read and run: for toolchains.
const READ_RUN: u32 = 0x0012_00A9;
/// The capability every container gets, which toolchain folders are opened to.
const TOOLS: &str = "athenaConnectToolchains";
/// Network: downloads (cargo, npm, pip) and dev servers. Not this computer's local network.
const NETWORK: &[&str] = &["internetClient", "internetClientServer"];
const DRIVE_REMOTE: u32 = 4;
const ALREADY_EXISTS: i32 = 0x8007_00B7_u32 as i32;

pub fn support() -> Support {
    Support::Available
}

/// The folder's own container: the same name every time for the same folder.
pub fn container_name(folder: &Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in folder.to_string_lossy().to_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("athena-connect.{h:016x}")
}

/// Program folders under your user folder (from PATH), plus Rust's.
pub fn toolchains() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return vec![] };
    let depth = home.components().count();
    let mut out: Vec<PathBuf> = [".cargo\\bin", ".rustup"].iter().map(|d| home.join(d)).filter(|p| p.is_dir()).collect();
    for p in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let lower = p.to_string_lossy().to_lowercase();
        // Only folders well inside your user folder (never the user folder or AppData itself), and not app aliases.
        if p.starts_with(&home) && p.components().count() >= depth + 2 && p.is_dir() && !lower.contains("windowsapps") && !lower.ends_with("\\appdata\\local") && !lower.ends_with("\\appdata\\roaming") && !out.iter().any(|x| p.starts_with(x)) {
            out.push(p);
        }
    }
    out
}

/// Before the first command: check the folder's drive can enforce permissions, create the
/// container, and open the folder, the temp folder and toolchains to it.
pub fn prepare(fence: &Fence) -> Result<(), String> {
    let folder = fence.read_write.first().ok_or("no folder")?;
    let name = fence.container.as_deref().ok_or("no container")?;
    unsafe {
        check_volume(folder)?;
        let sid = create_container(name)?;
        for p in &fence.read_write {
            grant(p, sid.0, FULL).map_err(|e| format!("couldn't open {} to the sandbox: {e}", p.display()))?;
            label_low(p).map_err(|e| format!("couldn't let the sandbox write in {}: {e}", p.display()))?;
        }
        let tools = capability(TOOLS)?;
        for p in &fence.read_only {
            // A toolchain folder we can't change (installed by an administrator) just stays unreadable to commands.
            let _ = grant(p, tools.0, READ_RUN);
        }
    }
    Ok(())
}

/// When the session ends: remove the container's profile (its own storage). The temp folder goes with the session.
pub fn release(fence: &Fence) {
    if let Some(name) = &fence.container {
        unsafe {
            DeleteAppContainerProfile(wide(name).as_ptr());
        }
    }
}

/// Run `command` in the folder's container (the helper does this); returns its exit code.
pub fn run(fence: &Fence, command: &str) -> i32 {
    match unsafe { run_confined(fence, command) } {
        Ok(code) => code,
        Err(e) => {
            // Never run a command we couldn't confine.
            eprintln!("athena-connect: refusing to run the command, because it couldn't be confined to the folder: {e}");
            126
        }
    }
}

/// The shell's command line for a command. PowerShell runs it from a script file in the session's temp folder
/// (returned, to remove afterwards): that gives plain-text errors (an encoded command reports them as XML).
fn command_line(shell: &str, command: &str, folder: &Path, temp: Option<&Path>) -> Result<(String, Option<PathBuf>), String> {
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    if shell == "cmd" {
        return Ok((format!("\"{system}\\System32\\cmd.exe\" /d /s /c \"{command}\""), None));
    }
    let temp = temp.ok_or("no temp folder for the script")?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let script = temp.join(format!("command-{}-{stamp}.ps1", std::process::id()));
    // Native programs' exit codes come back; UTF-8 with a BOM, which Windows PowerShell needs to read it as UTF-8.
    // Its core modules are loaded up front: loading them on first use doesn't happen in a container.
    // It starts in the folder by its full path: on its own it checks the folders above, can't see them, and starts in C:\.
    // The exit code is a failure if anything in the command failed, not only the last part.
    let folder = folder.display().to_string().replace('\'', "''");
    let body = format!(
        "$ProgressPreference='SilentlyContinue'\r\nImport-Module Microsoft.PowerShell.Management, Microsoft.PowerShell.Utility, Microsoft.PowerShell.Security -ErrorAction SilentlyContinue\r\ntry {{ [Console]::OutputEncoding=[Text.Encoding]::UTF8 }} catch {{}}\r\nSet-Location -LiteralPath '{folder}'\r\n$Error.Clear()\r\n$LASTEXITCODE = 0\r\n{command}\r\nif ($LASTEXITCODE) {{ exit $LASTEXITCODE }}\r\nif (-not $? -or $Error.Count) {{ exit 1 }}\r\n"
    );
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(body.as_bytes());
    std::fs::write(&script, bytes).map_err(|e| format!("couldn't write the command's script: {e}"))?;
    let line = format!("\"{system}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\"", script.display());
    Ok((line, Some(script)))
}

unsafe fn run_confined(fence: &Fence, command: &str) -> Result<i32, String> {
    let name = fence.container.as_deref().ok_or("no container")?;
    let folder = fence.read_write.first().ok_or("no folder")?;
    let container = derive_container(name)?;
    let mut caps = vec![capability(TOOLS)?];
    for n in NETWORK {
        caps.push(capability(n)?);
    }
    let mut attrs: Vec<SID_AND_ATTRIBUTES> = caps.iter().map(|c| SID_AND_ATTRIBUTES { Sid: c.0, Attributes: SE_GROUP_ENABLED as u32 }).collect();
    let sc = SECURITY_CAPABILITIES { AppContainerSid: container.0, Capabilities: attrs.as_mut_ptr(), CapabilityCount: attrs.len() as u32, Reserved: 0 };

    // The command gets this helper's input and output (the connector's pipes) and nothing else it holds.
    let mut handles: Vec<HANDLE> = Vec::new();
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let h = GetStdHandle(which);
        if !h.is_null() && h != INVALID_HANDLE_VALUE && !handles.contains(&h) {
            SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
            handles.push(h);
        }
    }

    let mut size = 0usize;
    InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size);
    let mut buf = vec![0u8; size];
    let list = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
    if InitializeProcThreadAttributeList(list, 2, 0, &mut size) == 0 {
        return Err(last_error("InitializeProcThreadAttributeList"));
    }
    let _cleanup = Guard(|| DeleteProcThreadAttributeList(list));
    if UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize, &sc as *const _ as *const c_void, std::mem::size_of::<SECURITY_CAPABILITIES>(), null_mut(), null()) == 0 {
        return Err(last_error("the container attribute"));
    }
    if !handles.is_empty() && UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, handles.as_ptr() as *const c_void, handles.len() * std::mem::size_of::<HANDLE>(), null_mut(), null()) == 0 {
        return Err(last_error("the handle list"));
    }

    let mut si: STARTUPINFOEXW = std::mem::zeroed();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    si.StartupInfo.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
    si.StartupInfo.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    si.lpAttributeList = list;

    // Everything the command starts stays in this job, and goes when the job does.
    let job = CreateJobObjectW(null(), null());
    if job.is_null() {
        return Err(last_error("CreateJobObject"));
    }
    let _job = Guard(|| {
        CloseHandle(job);
    });
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits as *const _ as *const c_void, std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32) == 0 {
        return Err(last_error("the job's limits"));
    }

    let (line, script) = command_line(fence.shell.as_deref().unwrap_or("powershell"), command, folder, fence.read_write.get(1).map(|p| p.as_path()))?;
    let _script = Guard(|| {
        if let Some(s) = &script {
            let _ = std::fs::remove_file(s);
        }
    });
    let mut line = wide(line);
    let cwd = wide(folder);
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    if CreateProcessW(null(), line.as_mut_ptr(), null(), null(), 1, EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED, null(), cwd.as_ptr(), &si.StartupInfo, &mut pi) == 0 {
        return Err(last_error("starting the shell in the sandbox"));
    }
    if AssignProcessToJobObject(job, pi.hProcess) == 0 {
        let e = last_error("AssignProcessToJobObject");
        TerminateProcess(pi.hProcess, 1);
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
        return Err(e);
    }
    ResumeThread(pi.hThread);
    CloseHandle(pi.hThread);
    WaitForSingleObject(pi.hProcess, INFINITE);
    let mut code = 1u32;
    GetExitCodeProcess(pi.hProcess, &mut code);
    CloseHandle(pi.hProcess);
    Ok(code as i32)
}

// --- helpers ---------------------------------------------------------------

struct Guard<F: FnMut()>(F);
impl<F: FnMut()> Drop for Guard<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

/// A SID and how to free it.
struct Sid(PSID, bool);
impl Drop for Sid {
    fn drop(&mut self) {
        unsafe {
            if self.1 {
                FreeSid(self.0);
            } else {
                LocalFree(self.0);
            }
        }
    }
}

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

fn last_error(what: &str) -> String {
    format!("{what} failed (Windows error {})", unsafe { GetLastError() })
}

unsafe fn create_container(name: &str) -> Result<Sid, String> {
    let n = wide(name);
    let desc = wide("Commands Athena runs through athena-connect");
    let mut sid: PSID = null_mut();
    let hr = CreateAppContainerProfile(n.as_ptr(), n.as_ptr(), desc.as_ptr(), null(), 0, &mut sid);
    if hr == ALREADY_EXISTS {
        return derive_container(name);
    }
    if hr < 0 {
        return Err(format!("couldn't create the sandbox (error {hr:#x})"));
    }
    Ok(Sid(sid, true))
}

unsafe fn derive_container(name: &str) -> Result<Sid, String> {
    let mut sid: PSID = null_mut();
    let hr = DeriveAppContainerSidFromAppContainerName(wide(name).as_ptr(), &mut sid);
    if hr < 0 {
        return Err(format!("couldn't find the sandbox (error {hr:#x})"));
    }
    Ok(Sid(sid, true))
}

unsafe fn capability(name: &str) -> Result<Sid, String> {
    let (mut groups, mut group_count, mut caps, mut cap_count) = (null_mut::<PSID>(), 0u32, null_mut::<PSID>(), 0u32);
    if DeriveCapabilitySidsFromName(wide(name).as_ptr(), &mut groups, &mut group_count, &mut caps, &mut cap_count) == 0 {
        return Err(last_error("DeriveCapabilitySidsFromName"));
    }
    for i in 0..group_count as usize {
        LocalFree(*groups.add(i));
    }
    LocalFree(groups as *mut c_void);
    if cap_count == 0 {
        LocalFree(caps as *mut c_void);
        return Err(format!("no capability {name}"));
    }
    let sid = *caps;
    for i in 1..cap_count as usize {
        LocalFree(*caps.add(i));
    }
    LocalFree(caps as *mut c_void);
    Ok(Sid(sid, false))
}

/// A drive without permissions (FAT, exFAT) or on the network can't keep a sandboxed command in the folder.
unsafe fn check_volume(folder: &Path) -> Result<(), String> {
    let p = wide(folder);
    let mut root = [0u16; 512];
    if GetVolumePathNameW(p.as_ptr(), root.as_mut_ptr(), root.len() as u32) == 0 {
        return Err(last_error("finding the folder's drive"));
    }
    if GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE {
        return Err("the folder is on a network drive, where Windows can't keep a command inside it".into());
    }
    let mut flags = 0u32;
    let mut fs = [0u16; 64];
    if GetVolumeInformationW(root.as_ptr(), null_mut(), 0, null_mut(), null_mut(), &mut flags, fs.as_mut_ptr(), fs.len() as u32) == 0 {
        return Err(last_error("reading the folder's drive"));
    }
    if flags & FILE_PERSISTENT_ACLS == 0 {
        let name = String::from_utf16_lossy(&fs[..fs.iter().position(|c| *c == 0).unwrap_or(0)]);
        return Err(format!("the folder's drive is formatted {name}, which has no permissions for Windows to enforce; use a folder on an NTFS drive"));
    }
    Ok(())
}

/// Give `sid` this access to a folder and everything in it (unless it already has it).
unsafe fn grant(path: &Path, sid: PSID, access: u32) -> Result<(), String> {
    let p = wide(path);
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let e = GetNamedSecurityInfoW(p.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, null_mut(), null_mut(), &mut dacl, null_mut(), &mut sd);
    if e != ERROR_SUCCESS {
        return Err(format!("reading its permissions failed (Windows error {e})"));
    }
    let _sd = Guard(|| {
        LocalFree(sd);
    });
    if has_grant(dacl, sid, access) {
        return Ok(());
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    GetSecurityDescriptorControl(sd, &mut control, &mut revision);
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W { pMultipleTrustee: null_mut(), MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE, TrusteeForm: TRUSTEE_IS_SID, TrusteeType: TRUSTEE_IS_UNKNOWN, ptstrName: sid as *mut u16 },
    };
    let mut new: *mut ACL = null_mut();
    let e = SetEntriesInAclW(1, &ea, dacl, &mut new);
    if e != ERROR_SUCCESS {
        return Err(format!("building its permissions failed (Windows error {e})"));
    }
    let _new = Guard(|| {
        LocalFree(new as *mut c_void);
    });
    // Keep the folder's own setting for inheriting from its parent exactly as it was.
    let mut info = DACL_SECURITY_INFORMATION;
    if control & SE_DACL_PROTECTED != 0 {
        info |= PROTECTED_DACL_SECURITY_INFORMATION;
    }
    let e = SetNamedSecurityInfoW(p.as_ptr(), SE_FILE_OBJECT, info, null_mut(), null_mut(), new, null());
    if e != ERROR_SUCCESS {
        return Err(format!("changing its permissions failed (Windows error {e})"));
    }
    Ok(())
}

/// Label a folder and everything in it low integrity, so a container may write there (unless it already is).
unsafe fn label_low(path: &Path) -> Result<(), String> {
    let p = wide(path);
    let mut sacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    if GetNamedSecurityInfoW(p.as_ptr(), SE_FILE_OBJECT, LABEL_SECURITY_INFORMATION, null_mut(), null_mut(), null_mut(), &mut sacl, &mut sd) == ERROR_SUCCESS {
        let low = has_low_label(sacl);
        LocalFree(sd);
        if low {
            return Ok(());
        }
    }
    // Low, inherited by everything inside, and only stopping writes from below low (nothing is below it).
    let mut label: PSECURITY_DESCRIPTOR = null_mut();
    if ConvertStringSecurityDescriptorToSecurityDescriptorW(wide("S:(ML;OICI;NW;;;LW)").as_ptr(), SDDL_REVISION_1, &mut label, null_mut()) == 0 {
        return Err(last_error("building the label"));
    }
    let _label = Guard(|| {
        LocalFree(label);
    });
    let (mut present, mut defaulted, mut new_sacl) = (0, 0, null_mut::<ACL>());
    if GetSecurityDescriptorSacl(label, &mut present, &mut new_sacl, &mut defaulted) == 0 || present == 0 {
        return Err(last_error("reading the label"));
    }
    let e = SetNamedSecurityInfoW(p.as_ptr(), SE_FILE_OBJECT, LABEL_SECURITY_INFORMATION, null_mut(), null_mut(), null(), new_sacl);
    if e != ERROR_SUCCESS {
        return Err(format!("setting its integrity label failed (Windows error {e})"));
    }
    Ok(())
}

unsafe fn has_low_label(sacl: *mut ACL) -> bool {
    if sacl.is_null() {
        return false;
    }
    let mut low: PSID = null_mut();
    if ConvertStringSidToSidW(wide("S-1-16-4096").as_ptr(), &mut low) == 0 {
        return false;
    }
    let _low = Guard(|| {
        LocalFree(low);
    });
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(sacl, &mut info as *mut _ as *mut c_void, std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation) == 0 {
        return false;
    }
    for i in 0..info.AceCount {
        let mut ace: *mut c_void = null_mut();
        if GetAce(sacl, i, &mut ace) == 0 {
            continue;
        }
        // A label ACE has the same layout as an allowed ACE: header, mask, SID.
        let a = ace as *const ACCESS_ALLOWED_ACE;
        let flags = (*a).Header.AceFlags as u32;
        if (*a).Header.AceType as u32 == SYSTEM_MANDATORY_LABEL_ACE_TYPE && flags & INHERITED_ACE == 0 && flags & SUB_CONTAINERS_AND_OBJECTS_INHERIT == SUB_CONTAINERS_AND_OBJECTS_INHERIT && EqualSid(&(*a).SidStart as *const u32 as PSID, low) != 0 {
            return true;
        }
    }
    false
}

unsafe fn has_grant(dacl: *mut ACL, sid: PSID, access: u32) -> bool {
    if dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(dacl, &mut info as *mut _ as *mut c_void, std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation) == 0 {
        return false;
    }
    for i in 0..info.AceCount {
        let mut ace: *mut c_void = null_mut();
        if GetAce(dacl, i, &mut ace) == 0 {
            continue;
        }
        let a = ace as *const ACCESS_ALLOWED_ACE;
        let flags = (*a).Header.AceFlags as u32;
        if (*a).Header.AceType as u32 == ACCESS_ALLOWED_ACE_TYPE && flags & INHERITED_ACE == 0 && flags & SUB_CONTAINERS_AND_OBJECTS_INHERIT == SUB_CONTAINERS_AND_OBJECTS_INHERIT && (*a).Mask & access == access && EqualSid(&(*a).SidStart as *const u32 as PSID, sid) != 0 {
            return true;
        }
    }
    false
}
