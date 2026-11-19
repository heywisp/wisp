use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::process::{Child, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::Pid;

use super::{
    POWERSHELL, Running, Tools, grouped_command, kill_all, powershell_args, spawn_group, utf16le,
};

const ACK: u8 = 6;
const END: char = '!';
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @'
using System;
using System.Text;
using System.Threading;
using System.Windows.Forms;
public static class WispClipboard {
    public static void Run(int ack, char end) {
        using (var dispatcher = new Control()) {
            var handle = dispatcher.Handle;
            new Thread(() => {
                try {
                    string line;
                    while ((line = Console.ReadLine()) != null) {
                        if (line.Length == 0 || line[line.Length - 1] != end) break;
                        var text = Encoding.UTF8.GetString(Convert.FromBase64String(line.Substring(0, line.Length - 1)));
                        dispatcher.Invoke((Action)(() => Clipboard.SetDataObject(text, true, 4, 10)));
                        Console.Write((char)ack);
                        Console.Out.Flush();
                    }
                } catch (Exception) {
                } finally {
                    dispatcher.BeginInvoke((Action)Application.ExitThread);
                }
            }) { IsBackground = true }.Start();
            Application.Run();
        }
    }
}
'@
"#;

pub(super) struct Helper<'a> {
    child: Child,
    leader: Option<Pid>,
    running: &'a Running,
}

impl<'a> Helper<'a> {
    pub(super) fn start(tools: &Tools, running: &'a Running) -> Option<Self> {
        let program = tools.find(POWERSHELL)?;
        let script = format!("{SCRIPT}[WispClipboard]::Run({ACK}, '{END}')");
        let encoded = wisp_media::encode_base64(&utf16le(&script));
        let mut command = grouped_command(&program, tools);
        command
            .arg("-Sta")
            .args(powershell_args(&encoded))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (child, leader) = spawn_group(&mut command, running).ok()?;
        let helper = Self {
            child,
            leader,
            running,
        };
        let stdin = helper.child.stdin.as_ref()?;
        fcntl_setfl(stdin, fcntl_getfl(stdin).ok()? | OFlags::NONBLOCK).ok()?;
        Some(helper)
    }

    pub(super) fn copy(&mut self, text: &str, timeout: Duration) -> bool {
        let (Some(stdin), Some(stdout)) = (&mut self.child.stdin, &mut self.child.stdout) else {
            return false;
        };
        let mut frame = wisp_media::encode_base64(text.as_bytes());
        frame.push(END);
        frame.push('\n');
        let deadline = Instant::now() + timeout;
        let mut input = frame.as_bytes();
        while !input.is_empty() {
            if !ready(stdin, PollFlags::OUT, deadline, self.running) {
                return false;
            }
            match stdin.write(input) {
                Ok(0) => return false,
                Ok(n) => input = &input[n..],
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
                Err(_) => return false,
            }
        }
        let mut ack = [0];
        ready(stdout, PollFlags::IN, deadline, self.running)
            && stdout.read_exact(&mut ack).is_ok()
            && ack == [ACK]
    }
}

impl Drop for Helper<'_> {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        if self.running.stopped.load(Ordering::SeqCst)
            && let Some(stdout) = &self.child.stdout
        {
            let mut fds = [PollFd::new(stdout, PollFlags::empty())];
            poll(
                &mut fds,
                Some(&Timespec {
                    tv_sec: 0,
                    tv_nsec: 250_000_000,
                }),
            )
            .ok();
        }
        let mut group = self.running.lock();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            kill_all(self.leader, &mut self.child);
        }
        *group = None;
    }
}

fn ready(fd: &impl AsFd, flags: PollFlags, deadline: Instant, running: &Running) -> bool {
    let mut fds = [PollFd::new(fd, flags)];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if running.stopped.load(Ordering::SeqCst) {
            return false;
        }
        let Ok(wait) = Timespec::try_from(left.min(Duration::from_millis(100))) else {
            return false;
        };
        match poll(&mut fds, Some(&wait)) {
            Ok(0) | Err(Errno::INTR) => {}
            Ok(_) => return fds[0].revents().contains(flags),
            Err(_) => return false,
        }
    }
    false
}
