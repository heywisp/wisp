# Running sessions

Every full-screen chat with a session log runs in its own host process. Close
its terminal, lose an SSH connection or open it in another window: the chat,
its agents and their programs keep working. There is no idle timeout or setting.

`wisp --resume` shows a running chat as `kept open` and always asks before opening
one. Pick it, or use `wisp --resume <id>` from any folder. Opening takes the chat
from its previous window; that window returns to the shell with the command
that brings it back. A newer install can open an older chat without restarting
it. Use `wisp restart` to restart the chats kept open on the newest version,
or `wisp restart <id>...` for only those chats.
A contract this client cannot read offers a restart before opening: No is under Enter, and No or cancel
leaves it untouched. Yes ends the old process, waits for its log to be released,
then opens it from that log in a new process. Restarting stops any current turn
and may rebuild each agent's cache. Off a terminal or inside a Wisp job there is
no restart question; the incompatible chat is left alone.

`/leave` returns to the shell and prints the command that brings the chat back;
it ends no turn, agent or program. `/quit`, `/exit` or two interrupts quit the
chat, ending its agents and programs. `wisp quit <id>...` does the same from
another terminal, without opening the chat. A prefix is enough from any folder.
It leaves a chat open in a window alone, whether that chat is hosted or lives
in its window; quit it there. Inside a Wisp job `wisp quit` quits nothing.
Switching with `/resume` leaves a hosted chat running. A host's termination
signal also quits it; a repeated termination signal forces the pending end. A machine restart, a logout that kills user processes,
a killed host or WSL shutting down ends it anyway. Resume then recovers its log.
A host loss found when the link drops or while reconnecting shows the last five
lines of its error file, when present, before the closing block and resume command.
A window whose connection is cut
reconnects automatically and says "Reconnecting to the chat. Press ctrl-c to close
this window." It waits 1 s between attempts, doubling up to 30 s, and keeps trying
while the host's socket is present and listening. Repeated cuts increase the wait;
a window that stays open for 30 s resets it. Ctrl-C during recovery closes only
the window and prints the command that opens the chat again, followed by any
text only that window held under `Not sent:`. A window that loses its chat because
it was taken, ended unexpectedly or was quit from outside prints that window-only
text too, below its closing lines. A main-chat message it saw accepted is not printed:
the chat returns it on the next open. After reconnecting, the history has one note:
"Reconnected to the chat." This is an ordinary opening:
it writes no record, makes no model request and changes no cached prefix. A
missing or refused socket means the host is gone: recovery stops and prints the
unexpected-end block and resume command, followed by any window-only text under
`Not sent:`, without starting another host. A missing
session log does not prevent recovery of a running host. A reconnecting window
takes the chat back from a window opened meanwhile. Quitting, a taken window and
an ended chat never trigger reconnection. Unsent text in the main chat and an open
question's unfinished answers come back after reconnecting. A message sent before
a cut but never confirmed comes back in the composer only if the chat lacks its
id; it is never sent again by itself. After a cut or restart, a child agent's text
comes back in its box if it is still running, or goes to main's history if it ended.
A closing window prints each child's text under `To <agent>:`.

A chat with logging off, `--resume-unlocked`, a socket path too long, or a host
that could not start lives in its window instead. Its top says why. Closing that
window or switching away from it ends that chat, as before. `/leave` instead
repeats that reason and does not close the window.

`wispd` is the kernel-independent session host library. It serves command and
event channels; `wisp-client::link` supplies Rust clients. A client that is not
the TUI can start a host with `wisp --host`, the ordinary chat flags and the
process-start interface below; it needs no terminal.

## Restarting

`wisp restart` finds the chats kept open, by title, without taking their windows.
It asks each to finish its current reply and tool calls, then starts that chat
again as soon as nothing is left running. A chat with nothing running restarts
at once. Processes that keep running after Wisp exits are left alone; a chat
that lives in its window is not listed or restarted.

`wisp restart <id>...` restarts only those chats, each named as `wisp quit`
names it: an id or its start, from any folder. The other chats are left as they
are. A bad id (one that matches no chat, matches several or isn't an id), or the
id of a chat that ended, lives in its window or runs this command, prints why;
the other named chats still restart, and the command ends with an error.

The command waits up to five minutes. Enter stops what is still running and
restarts those chats now. Ctrl-C before that leaves the chats still finishing
as they were; chats already restarted stay restarted. After Enter or the limit,
Ctrl-C and a closed shell do not stop the restart. The end names stopped work.
Every agent whose turn was stopped carries on, except the main agent's first
reply to a message: that chat waits for the message again. Wisp never starts a
stopped process again by itself. Idle keep-warm agents stay marked and warm.

Each chat starts its own next host, keeping its folder, environment, flags and
account. The newest version is the program now at the path that chat started
from. A chat started from a different, still-older program stays marked
`older version` and does not count as restarted on the newest version.
The daemon, if running, restarts last, unless chats were named; a stopped
daemon stays stopped.

Chats started before restart support cannot finish first. One question lists
those chats before anything changes; No, Esc, Ctrl-C or no terminal restarts
nothing. Yes stops and starts them with the shell's environment and the usual
settings, keys file and keychain, so a chat started with its own files may use
a different account. Their older windows close and print their resume commands.
Other open windows stay open: the bottom row says `About to restart`, then the
window reconnects. Messages accepted while finishing wait `after restart` and go
out again as new inputs once the chat answers, only if their window stays open
through the restart. With no window open, waiting messages return to the
composer with their images when the chat is next opened. Messages not kept in a
stopped reply also come back. A message whose acceptance was not confirmed returns
to the composer instead. Messages to another agent are withdrawn as usual. Nothing
already committed is sent again. A cancelled restart adds one note and the chat
carries on.

An empty window replaces itself with the newest program at its start-time path.
A window with unsent text keeps it and stays on its older version; once the
text is sent, `/leave`, then `wisp --resume` updates the window. Both show a
restarted note. Ctrl-C while waiting messages go out again stops the sending
and keeps the window open, with unsent text back in the composer. If the main
agent's open question was interrupted, the note says to message its answer if
the agent doesn't ask again; the main replay does not include child questions.
An agent's own restart notice appears as `↻ Wisp restarted`.

A chat that says nothing for ten seconds is `not answering`. A stopped chat has
up to sixty seconds to answer again; an early start failure shows its reason
and `wisp --resume <id>`. A window whose chat doesn't start again closes with
the failure and resume command, then any text only it held under `Not sent:`.
A chat that never answers is left running and named. A chat whose connection
fails while its log is still held is `still running`, with the reason.
Off a terminal, each changed line is printed once and no key is read. Inside
a Wisp job the command restarts nothing; with the job mark cleared, `wisp
restart` with no id still refuses the chat it runs in and restarts nothing.
A row for a chat with no title shows its short id. Names, descriptions and
error lines are cleaned for display; line-mode output can be redirected to a
file.

## The daemon

A hosted chat's start or opening also starts Wisp's daemon, unless it was stopped
by command or Wisp is running inside a job. It never ends for being idle, losing
its last client or having no chats. It holds no chat state and runs no kernel:
it only lists, starts and relays. Terminals still connect straight to their chats.

`wisp daemon` changes nothing. It prints `running`, `stopped` or `not started`,
then every chat kept open with its title and short id. A chat with no title is
named by its folder; `in a window` and `older version` mark those chats. The list
comes from chat sockets, even with the daemon stopped; a chat that refuses the
caller is listed by id alone. Inside a Wisp job no log is read; chats that refuse
the job are listed by id alone too, so other chats' messages never enter its output.
`wisp daemon start` starts it, or says it is already running.
`wisp daemon stop` stops no chat. It disconnects only clients using the daemon,
and leaves `<data>/daemon/stopped` so the next chat cannot silently restart it.
An explicit start removes that mark. A refused stop leaves no mark.

The daemon uses `<data>/daemon/daemon.sock`, its 0700 folder and 0600 socket.
`link::daemon_dir` and `link::daemon_socket` name these paths. Its lifetime lock
is `daemon.lock`; a dead daemon's socket is replaced on its next start. The
private `daemon.err` is opened for appending by a launcher and emptied only by
the process holding that lock, so a second launch cannot erase its errors.
Its hidden `wisp daemon serve` writes one `Ready::Socket` and serves even if
nobody reads that line. A termination signal stops it; hangups and interrupts do not.

The local interface uses `ToDaemon` and `FromDaemon`, one JSON object per line:

| Send | Receive |
|---|---|
| `Hello { daemon }` | `Welcome { daemon }` or `Refused { daemon }` |
| `Chats` | `Chats { running }`, sorted session ids of live sockets |
| `Start { folder, resume }` | `Started { session }` or `NotStarted { ready }` |
| `Open { session }` | `Linked` or `NotRunning` |
| `Stop` | connection closes when the daemon stops |

`DAEMON` versions this interface, independently of `CONTRACT`. `Hello`, `Welcome`,
`Refused` and `Stop` have the permanent-frame fixture rule of the link below;
a client of another version can still send `Stop`. An undecodable request gets
`Unknown`, containing none of its text. A version refusal changes nothing else.
`Start` accepts only an absolute existing folder and a `valid_id` resume id;
`Open` uses only a name discovered in the run folder, never a caller's path.

After `Linked`, the client sends the chat's own `Hello` and speaks its link on
the same connection. The daemon forwards already-buffered bytes first, then
copies fixed-size buffers both ways until either side closes. It reads no
relayed frame and logs none. There is no daemon/chat version agreement: the
chat and client check their own contract end to end. `wisp-client::link::daemon`
returns a Rust connection with `chats`, `start`, `open` and `stop` methods;
`link::stop` can stop a daemon of another interface version too.

Stopping and restarting the daemon drops no turn, agent or program. Reopening
through it catches up exactly as a direct opening does: history through the
last completed turn, current state (an open posted question included), events
since that cut, then live events.
This writes no record, makes no model request and changes no cached prefix.
A client must reconnect itself; the daemon retains no queue or client state.

Chats started through the daemon inherit its environment: the shell that
started it, not the connecting client's shell. Stop it and start it from the
right shell to replace that environment. The executable path is read once at
its own start; after reinstalling Wisp it starts chats with the installed
program. Existing chats keep their original process until `wisp restart`, which
with no id restarts each chat itself and then the daemon. Stopping and starting
only the daemon moves it to the new version without restarting any chat. It reaps
every child it starts, and stores no child registry. No login service or remote
listener is installed; after a crash or reboot the next hosted chat starts it.

## Starting and finding a host

A starter binds a private socket with `wispd::bind(data)` before it starts a
session. After acquiring the session's log lock it calls `Socket::publish(id)`.
Publication replaces a stale socket; the log lock prevents two hosts serving
one session. Dead private socket names are swept at the next bind. Dropping the
socket removes its name.

`link::run_dir(data)` is `<data>/run`; `link::socket(data, id)` is the session's
socket there. `link::error_file(socket)` names the host's error file beside its socket.
`link::session_of(path)` accepts only `.sock` names whose stem
passes `wisp_protocol::valid_id`. Private bind names cannot be mistaken for
chats. List these names and send `Hello` to find running chats without opening
one. `Welcome` gives its session, working folder and whether a window has it.

The process-start interface uses one JSON `Start` line on stdin: `asks`,
`terminal`, `account` and `follow`. Stdout carries `Ready` lines: any number of
`Notice`s, then one `Socket`, `Window`, `Failed` or `Ask`. A window fallback is
not retried silently. The host process uses the same executable, folder and
environment as its starter; installing a newer executable does not update
sessions that are already running.

## The link

One JSON object per line, encoded by `wisp_protocol::jsonl::line`. Link frames
use `type` and an optional `data` object; all types are in `wisp_protocol::link`.
Client frames are `Hello`, `Open`, `Command`, `End`, `EndIfWindowless` and `Restart`.
Host frames are `Welcome`, `Refused`, `Opened`, `Event`, `Taken`, `WindowOpen`,
`Restarting` and `Ended`.

| Send | Receive |
|---|---|
| `Restart { wait: true }` | live `Event(Restart)` rows, then `Restarting` and `Ended` |
| `Restart { wait: false }` | stop now, then `Restarting` and `Ended` |

`Welcome.restart` defaults to false when absent. Only a host sends `GetReady`
to the main agent; a window's `Command(GetReady)` is refused. A second restart
peer gets `Rejected`. If the requesting peer leaves before the stop, the host
cancels getting ready. Restart rows go only to that peer and the window, never
to the replay. A window opened while finishing gets the latest row, aged since
it was observed. These additive frames leave `CONTRACT` and `DAEMON` at 1.

`Hello { contract, build }` changes nothing. `Open` takes the chat from its
previous window, which gets `Taken`. A lost connection leaves the chat running;
only a connection that had opened it causes `WindowGone`. Probes, including a
connection sending no frame, do not step a window out of a terminal program.
No idle timer ends a session.

`CONTRACT` identifies the link's meanings, separately from Wisp's version and
its executable's modification-time `build`. An incompatible contract gets
`Refused`, but `End` still works, even without `Open`. Unknown lines are skipped
by a client; the host answers one with `Rejected`, without any of its text.

`Hello`, `Welcome`, `Refused` and `End` are permanent. Their golden fixtures
pin their bytes. Re-bless a permanent frame only when the new fixture adds
optional fields: never remove, rename or reinterpret an existing field.
Additions elsewhere need no contract change; changed meanings do. One
accepted exception: non-blocking posted questions (log version 40) keep
`CONTRACT` at `1`, because bumping it would refuse every older hosted chat and
force a restart that stops its turn and may rebuild its cache. A newer window
on an older host sees only old events; an old window kept open through
`wisp restart` closes a posted card at turn end and skips `withdrawn` closes
until reopened.

A new host sends a provider-off `LeaveNeeds` with both the legacy `ways` (agents wait) and `with_followers`. A new window uses `AnswerProviderSwitch` only when `with_followers` is present and nonempty; otherwise it uses the existing unsaved settings change, then saves the switches after the host confirms it. An old window may show `LogoutNeeds { fixed: true }` as a one-row Where step. Its old `Logout` answer cannot approve a new metered move without `accept_api`. A new window's forced `/logout all` preview fails closed if an old host rejects `preview_logout`; the window shows the refusal and never retries it as executable `Logout`.

Each connection gets a distinct `REQUEST_RANGE` beginning at
`Welcome.first_request`. Clients count up within that range. The host counts
down from `HOST_FIRST`; its description answers never reach a window.

`dial` connects a local socket and checks its owner's uid. `handshake` speaks
`Hello` on any supplied byte stream; `connect` combines the two. `open` returns
a `Link` with the opening, view, current settings and ordinary `Channels`.
Its `ended` watch distinguishes `Taken`, `Restarting`, `Ended { saved }`, a
window cut from a running chat (`Cut`) and a lost link. After a lost connection, a link opened with
`connect` asks the host whether the chat still runs only while the window still
takes its events, allowing 5 s each for connecting and the welcome. A host
with another window means `Taken`, one without a window means `Cut`; a missing
or refused socket means a lost link. A slow or silent socket remains `Cut`, not
proof that the host ended. Reconnect attempts allow 30 s for connecting and the
welcome; after the welcome, opening has no client deadline, as on first attach.
A link opened with `handshake` alone or through the daemon cannot make this check.
Dropping it leaves the session. `Link::end` sends `End` and waits without a
client-side deadline; `Connection::end` can end a chat without opening it.
`end` sends the same fixed frame on a raw stream, even after `Refused`, and
waits for `Ended`; the first restart of an older host uses that wait. `Connection::end_if_windowless`
returns `Some(saved)` after ending or `None` if a window has the chat.

The host's `Host::run` owns its event drain and socket admission. `serve` speaks
the same link on any already-admitted byte stream using `Host::handle`.

External `wisp quit ID...` sends `EndIfWindowless` for each chat. The host checks
window ownership and decides whether to end in the same event: `WindowOpen`
refuses without changing the chat; otherwise shutdown proceeds to `Ended`.
Inside-window quit and the first restart of an older host use unconditional `End`.
A restart-capable host instead accepts `Restart` from its one restart peer.
Older clients keep
their existing `End` behaviour. An older host rejects the unknown conditional
frame with an `Event(Rejected)`; the new client then uses the legacy
`Welcome.window` check and ordinary `End`. That fallback keeps older hosts usable,
but the reopening race remains for chats whose host started before the upgrade.

On `End` or an accepted `EndIfWindowless`, `run` closes the command channel and
returns `Shutdown`.
`Handle::end` requests the same transition internally, without a socket or
admission check; host termination signals use it, including forced repeats. Pass the
session's final join/save future to `Shutdown::finish`: it drains kernel events
and watches later forced ends meanwhile. During a restart it routes those events
to the window before `Restarting`; an ordinary end still drops them. The future
must release the session's store and log lock before returning whether it saved. Only then is `Ended`
sent and the socket and error file removed. Requested shutdown also waits
for peer writers to finish, within one shared write timeout, before the host
runtime stops; a non-reading peer cannot hold it open. A false save result, or a lost
link during a requested quit, exits with the same forced status as an in-window
shutdown; a waiting settings save gets the same save-cut note. An unrequested kernel stop closes
the link without inventing a successful end; its error file stays.

## Opening

The start step returns `Opening`: version, folders, log, account, model,
effort, cache time, limits, prices, instructions and search facts. Accounts
use `EndpointSummary`; credentials are kinds, never values. `wisp` fills
`Opening.card` and `Opening.start.note`. `Opening.start` also carries warnings
and a `Continued` account change; the TUI words that change from its facts.
Every chat window maps the opening through `wisp_render::chrome::Facts::from`.

The first window gets the start's `SessionView` once, including returned
composer inputs. Each returned input carries its original id when known; a
window that already holds it skips it. A client must not reuse the id of a
returned message that no window has taken. Later windows get `DescribeSession`'s
view, including returned messages no window took, current `Now`, then the state
as ordinary events, then each agent's events since its cut, then live events. The state includes current spend, context,
agents, plugins, keep-warm and live programs from every turn. An agent listed by
the state is not spawned again by replay. `Opening.start` is absent for these
later windows; `Now`, not the start's settings, is authoritative. A rejected
description closes that opening without sending the host's internal answer to
the window.

A host enables opening replay for its whole agent tree before starting it.
Only an opted-in session emits `InputAccepted { opening: true }` for a direct
idle send, before `TurnStarted`, without appending an acceptance log record.
The opt-in is internal, not a user setting, and changes no prompt or request.
Direct kernel clients and JSONL output keep their existing event bytes. The
voice layer ignores replay-only opening acknowledgements, in hosted sessions
too, so its provider requests are unchanged. This gives a
window opened mid-turn the original input text and images. A window that
already tracks that input ignores the opening event; a new window draws it
once, without a queued label. Other acceptances omit `opening` and keep their
queued or held meaning.

A before-turn hook may hold `DescribeSession` until it finishes.
A description captures the last completed turn's log cut and waiting inputs,
then reads and folds the log off the agent loop when the store has an owned
reader. Its events stop at the cut, while its input IDs cover every ID the
agent holds when asked, including accepted inputs not yet logged.
The input IDs also cover messages passed to the chat's agents this run.
The host keeps all events while any description is pending. On its answer it
omits main-agent events through the last turn already in the view, sends the
rest, then trims each agent back to its own last turn end. A turn ending during
the read is not lost or shown twice. Describing, opening and leaving write no
record and change no provider request; stepping out of a terminal preserves
the existing detach record. A window that restores or already holds returned
messages sends `ReturnedTaken { ids }`; the kernel drops those it holds and
writes one `returned_taken` record, with no provider request.

When an agent's next ordinary user turn starts, the host drops the returned
text from that agent's stored `Rewound`, so a window opened later still gets
the cut and divider but nothing in its box; returned text that was never
sent is then gone for a fresh window, as it is once any turn has ended.

Consecutive text, thinking and output deltas are joined in the
replay store. Each call and program keeps at most its last 64 KiB of output,
on UTF-8 boundaries, including interleaved output. A terminal program that
wrote more is redrawn only from the retained part until it redraws itself.
Private sign-in events are carried live only to the window whose request
started them. Replies that arrive after that window has gone or another
window takes over are dropped. They are never stored for another window.
A connection with more than 64 MiB of waiting live lines is closed on its next
send instead of blocking the session. Opening history is not counted, so a
long chat can still open. A write waits until it finishes or the connection is
stopped; shutdown shares one 1 s wait across all connections. The window's link
reads up to 16,384 events ahead of its screen. An event-channel closure is never
made into a turn end.

## Leaving

`WindowGone` forgets the window's shown sections, watch and usage subscriptions,
and steps out of its terminal program. It does not stop the turn or programs.
`WindowGone` also reaches the session's children. A closed command channel
instead ends the session after that same local cleanup; each child cleans up
when its own channel closes.

## Admission and secrets

The run folder is 0700 and sockets are 0600. `same_user` checks peer credentials
before the first frame, and the client checks the host's uid too. On Linux,
`admit` also refuses descendants of the host and job-marked peers, except the
host itself and its ancestors. This lets a job's client open the chat it starts.
Where the ancestry check cannot run, another connection is refused while a
window has the chat.
That fallback is checked again at `Open`, including for preconnected clients.
Generic `serve` streams use this fallback too, since they have no process
ancestry check. Checked local connections retain ordinary take-over. This is
a seatbelt, not a sandbox: a process that clears its job mark can pass it.

The daemon uses the same uid check before reading a frame. On Linux, both job
checks read `/proc/<peer pid>/environ` only to check whether `WISP_JOB` is present,
regardless of its value; an unreadable environment counts as job-marked and no
environment text is retained or shown. The daemon refuses all job-marked peers.
This prevents an agent's commands from listing, starting or opening chats through
the daemon or stopping it, even from another data folder or an in-window chat.
Both are seatbelts, not a sandbox: clearing the mark bypasses both job checks.
Off Linux the daemon has only the uid check and chats keep their fallback.

Secrets cross only in their existing protocol carriers (`Secret`), whose debug
form is redacted: account saves and edits, secret answers, pasted sign-in codes
and the private sign-in address. No frame is logged. No undecodable line is
returned in an error, response, notice or stderr. A host's private `.err` file
is for its own start and panic lines, not frames. Each window closed for falling
behind adds one line with the UTC time, the reason and the waiting-line bound,
never the line's contents. The host reopens stderr in append mode so a later
panic cannot overwrite these lines. A clean end removes the file.

`Host::with_outbound` keeps `wispd` independent of the kernel. `Host::send`
invokes the callback synchronously for opening, replay, live and restart-drain
batches, but not for `Shutdown::finish`'s direct `Restarting` and `Ended` controls.
Callbacks edit payloads in place or omit/replace a suppressed checklist-brief
`ToClient::Event` (with `LogDisabled` and/or a snapshot `Rejected`), preserving
unaffected frames' variants and order; omitting `publish` (such as on a
suppressed `ToClient::Opened`) closes the peer. They call `publish` once for
synchronous enqueue, holding any secret registry read guard without re-entering
the registry or waiting for dependent work; extra calls are ignored.

A chat that runs inside the `wisp` process has no host, so it applies the same
scrubbing where it hands events on: each JSONL line just before it is written
(also while draining at exit), each event the window's relay sends, and the
window's opening view once. Each is scrubbed against the secrets known at that
moment, so a secret Wisp learns while an event is still queued is hidden from
the fields the kernel's scrubber covers: checklist steps and sections, a
starting brief's checklist steps, hook audits, tool call displays and inputs,
restart rows' descriptions and targets, and process start and restore targets
(the rules are in `docs/session-log.md`, under "Checklist privacy" and "Hook
audit redaction", and in `docs/agents.md`, under "Getting ready for a restart").
Other fields are not rescanned here. An event already handed on is not
revisited, as on a host: the one event waiting in the relay's slot, and a view
already built for drawing. A JSONL line for such an event is written while the
secret registry is held for reading, so a reader that stops reading the output
also holds back a new secret's registration until that line is written. A
suppressed checklist brief is dropped or replaced by what the kernel emits, in
the JSONL output and the window alike. A suppressed opening view opens the
window without its events, where a host closes the peer. A window attached to a
host leaves all of this to the host.

A host still depends on the computer keeping it alive. A login service that
kills user processes at logout ends hosts, just as it ends tmux. Windows
stopping WSL also ends them and the daemon; keeping a terminal open has the same effect as
preventing WSL's idle stop. Nothing in the host overrides an operating-system
shutdown.

## Windows with WSL

Windows usually stops WSL 2 about 15 seconds after the last terminal closes,
ending every chat. Before opening a chat or its list, Wisp offers to add
`[general]` and `instanceIdleTimeout=-1` to the Windows user's `.wslconfig`.
Enter alone selects `No, don't ask again`; only a deliberate Yes changes it.
Escape cancels the start without remembering an answer. A first-run setup,
a command without a terminal and a Wisp job are never asked.

Wisp finds the user folder from the first existing WindowsApps entry in `PATH`.
It edits only readable UTF-8 files, not links; a missing file is created.
Other settings, line endings, a byte-order mark and the old mode are kept;
a new file uses 0644. A refused mode or folder sync falls back to write and
rename. In an authorized scratch-file probe at
`/mnt/c/Users/<user>/AppData/Local/Temp/...`, mode-setting was accepted without
preserving 0644, and file sync, rename and folder sync succeeded; the probe
file was removed. Windows-drive permission semantics may differ from Linux's.

After Yes, the chat shows a restart note while the file is newer than Linux's
start: run `wsl.exe --shutdown` once, here or in Windows. This ends everything
in Linux that one time. A failed write shows its error and the lines to add at
the top of the first chat that opens, and asks again on the next start. If the
line was written before a folder-sync error, only the restart note shows.

No leaves `<data>/wsl-declined`; remove that mark to be asked again. Where no
Windows user folder was found or the file is a link or not UTF-8, Wisp changes
nothing. Add the two lines above by hand and restart WSL once instead.
`wisp daemon` prints the warning or restart note first, even after No. Older
WSL versions without `instanceIdleTimeout` need updating first; leaving one
terminal open also prevents the idle stop.

A `wisp` run inside a Wisp job stays in that job's session and process group,
marked by `WISP_JOB=1`; it ends with the job instead of leaving a detached host.

A provider loads with its chat's plugins; the daemon holds none.
