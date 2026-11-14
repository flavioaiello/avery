<p align="center"><img src="assets/logo.png" width="128" alt="Avery"></p>

# avery

A private AI agent for macOS: [Goose](https://github.com/block/goose) Desktop, a local model served by
[Splash](https://github.com/incoai/splash), and a small mail and calendar bridge that reads your private mail and
calendar, writes drafts, sends mail and changes calendar events, but only after you click in a native macOS dialog.
The bridge uses the accounts you already keep in **Apple Mail and Calendar**: no registration, no app of your own, no
tokens.

Everything runs on your Mac. macOS keeps your credentials and Avery never sees them; the bridge also works offline.

> Status: early (v0.6.1). After installing, run `avery mail selftest` and `avery mail selftest-write`. They check the
> integration with Mail.app and Calendar.app, which the automated tests cannot do (they use demo data and a mock of the
> scripting interface).

## What is in the box

| Part | What it does |
|---|---|
| `Avery.app` | The icon in Spotlight and the Dock: starts the model, waits until it is ready and opens Goose with the Avery profile; while nothing is set up, its first start guides you through the setup in dialogs |
| `avery` | Command line: creates the Goose profile, starts the model and Goose, checks the setup |
| `avery-mail` | MCP server for Goose (stdio, Rust, no dependencies) that reads and changes Mail.app and Calendar.app through `osascript` |
| Goose profile | Config, permissions and a local provider for Splash, created by `avery init` in `~/.avery/goose`; your default Goose profile is not touched |

## Requirements

- Apple silicon Mac with macOS, Mail.app and Calendar.app with your private accounts
- [Homebrew](https://brew.sh); it builds the mail server from source with Rust (1.89 or newer, installed as a build
  dependency)
- Goose Desktop (cask `block-goose`) and Splash (`incoai/tap/splash`, installed as a dependency)

## Install

Goose Desktop is a cask, so Homebrew cannot install it as a dependency of a formula:

```sh
brew install --cask block-goose
brew tap flavioaiello/avery git@github.com:flavioaiello/avery.git
brew install --cask flavioaiello/avery/avery     # Avery.app in /Applications, plus the command line as a dependency
```

The cask installs `Avery.app` from a disk image. The command line alone is the formula `flavioaiello/avery/avery`.
If the repository is public, tap it with `https://github.com/flavioaiello/avery` instead.

macOS 26 or later is required for the cask (the app icon is made for it). The app is signed ad hoc and not notarized,
because there is no Apple Developer ID behind it; the cask removes the quarantine flag so that Gatekeeper lets it start.
Only install it from a tap you trust.

## First run

```sh
avery init                # create the Goose profile in ~/.avery/goose
avery pull                # download the model once (about 17 GB); stop it with Ctrl+C when it says Ready
```

Then open **Avery** (Spotlight, or `avery up`). While nothing is set up, the first start shows dialogs that guide you
through the setup (see [First start: the setup dialogs](#first-start-the-setup-dialogs)). The same step from a terminal:

```sh
avery mail setup          # lists your Mail accounts and calendars; choose a level per item: off, read or write
avery mail selftest       # check read access
```

Everything is off until you give it a level. `write` means: drafts and sending for a mail account, creating, changing
and deleting single events for a calendar, each only after your click in a macOS dialog:

```sh
avery mail selftest-write  # test the approval dialogs: a draft, a mail to yourself, a test event, an all-day test event
```

Restart Goose after changing accounts or permissions.

### Update from 0.5.0

0.6.0 reads only config format 4. A config file in any other format, including the format 3 of 0.5.0, is rejected and
never rewritten. Move it away and choose your accounts and calendars again:

```sh
mv ~/.avery/mail/config.json ~/.avery/mail/config.old.json
avery mail setup
```

Gmail and Outlook.com accounts can no longer be linked: 0.6.0 reaches mail and calendars only through Apple Mail and
Calendar. Add those accounts in Mail.app and Calendar.app (System Settings > Internet Accounts) and enable them in
`avery mail setup`. If you linked an account with 0.5.0, its sign-in is still in your Keychain (service `avery-mail`),
and 0.6.0 cannot delete it. Remove it yourself, one command per account, and revoke the app in your Google or Microsoft
account settings if you want the access gone there too:

```sh
security delete-generic-password -s avery-mail -a google:alice@gmail.example      # the account is <provider>:<address>
security delete-generic-password -s avery-mail -a microsoft:bob@outlook.example
```

The logs `sendlog.jsonl` and `changes.jsonl` keep working. The empty file `vault.lock` beside `config.lock` is no longer
used and can be deleted. If `avery doctor` reports profile files that do not ask before changes, or you want the new
wording of the mail extension, `avery init --force` writes new ones (the old ones are kept as backup).

## First start: the setup dialogs

When you open Avery (`Avery.app` or `avery up`) and nothing is set up yet, native macOS dialogs guide you through the
setup before Goose starts. They are a thin layer over the very same code as `avery mail setup` and
`avery mail selftest`: nothing is configured differently, and everything stays in `config.json` as described below.
There is no menu bar app and nothing keeps running.

1. **Welcome.** *Weiter* starts, *Später* asks again at the next start, *Nicht mehr fragen* never asks again (see below).
2. **Mail and Calendar.** It first explains that macOS asks once for permission to control Mail and Calendar, then lists
   your accounts and calendars. Choose the ones Avery may read, then which of them it may also write. A last dialog lists
   exactly what will be saved: one line for every grant that changes (not for the ones that stay as they are), the write
   grants first, under a count such as "3 Kalender zum Schreiben, 9 zum Lesen". Only *Ja* saves it. If the list does not
   fit into one dialog, it is not shown and cannot be confirmed: nothing is saved, and the next dialog says to use
   `avery mail setup` in a terminal.
3. **Summary.** What is set up, and the offer to run the read check (`avery mail selftest`) now and show its result.

Every dialog can be cancelled, and cancelling means: stop, nothing more is saved. What was completed before stays and the
last dialog says so. Every dialog also gives up after at most five minutes without an answer (the same as cancelling).

`avery mail onboard` shows the same dialogs whenever you want, also when everything is set up. `avery mail onboard
--if-needed` is what the launcher runs before Goose starts: it stays silent and does nothing when accounts or calendars are
enabled, after you chose *Nicht mehr fragen* (that creates the empty file `~/.avery/mail/onboard.dismissed`, owner-only:
delete it to be asked again), when `AVERY_NO_ONBOARD=1` is set (in the environment or in `~/.avery/avery.env`), in demo
mode and when there is no graphical session (for example over SSH). If the dialogs cannot be shown or are cancelled, Avery
still starts. `avery mail onboard` changes the configuration like `setup` and `revoke`, so it refuses in demo mode.

The dialogs come from the helper `/usr/bin/osascript` and show only on the Mac's screen; the agent can neither start
them nor answer them (the server `avery-mail serve` has no code path to them).
macOS may ask for permission to show notifications (System Settings > Notifications).
macOS asks once for Automation (Mail and Calendar), because Avery controls Mail and Calendar
(System Settings > Privacy & Security): allow what Avery should use.

If a step fails (for example macOS blocks the dialogs, the *Nicht mehr fragen* marker cannot be saved or `config.json`
cannot be read), the setup ends and says why: as `Fehler: ...` in a terminal and, where macOS shows notifications, as a
notification, also when `Avery.app` or `avery up` started it. If no dialog and no notification appears, run
`avery mail onboard` in a terminal: it shows the dialogs again or says why it cannot. Avery starts as usual in every
case. If you prefer the terminal, `AVERY_NO_ONBOARD=1` keeps the dialogs away and `avery mail setup` always works.

## Apple Mail and Calendar

Avery reads and changes your mail and events through Mail.app and Calendar.app, with whatever accounts they have
(iCloud, Gmail, Outlook.com, Exchange, IMAP, ...). Avery itself signs in to nothing: macOS holds the credentials.

`avery mail setup` is the first command that starts Mail and Calendar: it lists the Mail accounts and the calendars live,
and you choose a level per item (0 = off). macOS asks once to let the app that runs Avery control them (Terminal for the
command line, Goose for the agent). Your choices go into `config.json`. Afterwards the tools and `selftest` start Mail or
Calendar only for an account or calendar that is enabled; a command or tool with nothing enabled never starts them. An
account or calendar that vanished from Mail or Calendar is noticed by `setup`, by `selftest` and by tool errors. Setting
every item to 0, or `avery mail revoke`, switches everything off again.

Calendar.app lists a recurring event only once, so Avery expands the common repeat rules (daily, weekly, monthly,
yearly with the usual parts) itself; rules it cannot expand are shown once and flagged. Reading a Calendar.app calendar
scans all of its events (up to 20000), so very large calendars are slow. All times are local.

## Daily use

Open **Avery** with Spotlight (Cmd+Space) or drag it from the Applications folder to the Dock. It starts the model,
waits until it is ready and opens Goose with the Avery profile (while nothing is set up, the
[setup dialogs](#first-start-the-setup-dialogs) come first). If Goose is already open with that profile, it just
brings it to the front. When Goose has been closed for a few minutes (`AVERY_IDLE_MINUTES`, default 5), the model stops
by itself and frees its memory. There is nothing to quit and no second icon.

Without the cask, `avery install-app` creates the same app in `~/Applications`. It does nothing if the cask's app is
already in `/Applications`, so there is never a second icon.

The same works from a terminal: `avery up`. `avery down` stops the model right away.

Do not start Goose from its own Dock icon for this setup: that opens Goose's default profile, and the model is not running.

Only the model runs in the background; Goose Desktop is a normal app and starts the mail server itself when it needs it.
`avery up` uses `brew services run avery`, so the model is not started at login. A model that you started yourself, or
with `brew services start avery` (at login), is never stopped automatically. The model holds up to 28 GB of memory while
it runs. `avery model` runs it in the foreground instead (for testing). `avery doctor` shows what is missing and reports
a Goose configuration that cannot be parsed.

## How sending and changes are protected

The agent reads untrusted mail and calendar invitations, so the design assumes that they can try to talk the model into
doing something.

- **Levels.** Only the accounts and calendars you gave a level with `avery mail setup` are ever touched: `read` or
  `write`, set by you in a terminal or in the setup dialogs. The model cannot change them. Mail rights are per account,
  calendar rights per calendar.
- **Approval dialog.** A native dialog, shown by the server process and not by Goose, lists sender, recipients, subject
  and the exact text before a mail is sent. Every line of the text starts with `│` and a very long text is cut with a
  visible note, so that it cannot pass for a line of the dialog itself. Recipients are never shortened: a sent mail
  goes only to plain ASCII addresses of at most 254 characters. The server builds the mail in Mail.app and reads
  recipients, sender, subject and text again after your click. Only a click on *Senden* sends. Cancel, a timeout (75 s)
  or any error before the click means nothing is sent. If Mail reports an error or does not answer after the click, the
  outcome is unknown: the attempt stays in the log as unclear and counts as sent, and the model is told to ask you to
  check *Sent*. The model cannot operate the dialog.
- **Calendar changes** (`create_event`, `update_event`, `delete_event`) work on single events without attendees and
  without repetition. Events with attendees (a change would notify other people) and recurring events are refused.
  The dialog shows the event as it is in the calendar and what will change (before and after); the server reads the
  event again after your click and aborts if it differs. A failure after the click is reported as an unknown outcome.
  Notes are shown with `│` at the start of every line, the new notes first and the old ones cut short, so that text
  from an event can neither pass for a line of the dialog nor push the new notes out of view.
- **Goose asks too.** `permission.yaml` sets the sending and calendar change tools to `ask_before`.
- **Limits.** Mail: plain text only, no Bcc, no attachments, at most 5 recipients (also for replies to all), at most
  1500 characters and 40 lines per sent mail (longer texts must be drafts you send from your mail app), 5 sent mails per hour,
  30 drafts per hour. Calendar: title and place up to 200 characters, notes up to 1000 characters and 30 lines (the dialog shows all of them), 10 changes per hour. For both:
  6 attempts per 10 minutes and a 20 second cool-down after you decline. Every attempt is logged before anything
  happens (`avery mail sendlog`, `avery mail changes`). If a log cannot be written or read, contains a line the server
  does not understand, or is too large to check the last hour, nothing is sent or changed until you look at it and move
  the file away. The hourly limits can be changed in `config.json` (`sends_per_hour`, `changes_per_hour`, 1 to 30).
- **Undo.** The change log keeps the event as it was before and after every change. `avery mail undo` restores it
  (a deleted event is created again; attendees and alerts do not come back). It asks for the dialog again and is not
  held back by the hourly limits, which exist for the model.
- **Hidden text.** Control, format and invisible characters (zero-width characters, line and paragraph separators,
  Unicode tag characters, variation selectors, fillers) are stripped from mail and event text the model reads and from
  text it writes, so what the dialog shows is what is stored or sent.
- **No other destructive tools.** There is no code that deletes, moves or flags mail.
- **Replies** use Mail's reply: the recipients and the subject are taken from the original message.
- **No retries.** A request that fails after the click is reported as an unknown outcome and counts as sent; the server
  never repeats a send or a change on its own.

The dialog is the hard gate. Instructions to the model (`top-of-mind.md`) are only a second line of defence.
The profile disables Goose's shell and file extensions; do not enable them for this profile.

## Commands

| Command | Purpose |
|---|---|
| `avery init [--force]` | Create the profile (existing files are kept; `--force` replaces them and keeps a backup) |
| `avery mail onboard [--if-needed]` | The guided setup in macOS dialogs, over the same code as `setup` and `selftest`; `--if-needed` asks only while nothing is set up and not dismissed (what the launcher runs) |
| `avery mail setup` / `revoke` | Choose the level (off, read, write) per Mail account and calendar (listed live from Mail and Calendar) / remove all grants |
| `avery mail selftest` / `selftest-write` | Check read access / the approval dialogs, drafts, sending and a test event |
| `avery mail status` / `accounts` / `calendars` | Show configuration and levels, enabled accounts, enabled calendars (no Mail or Calendar access) |
| `avery mail sendlog` / `changes` / `undo [id]` | Recent send attempts / recent calendar changes / undo a calendar change |
| `avery install-app` | Create `Avery.app` in `~/Applications` without the cask (skipped when the cask's app is installed) |
| `avery up` / `avery down` | Start the model service and open Goose (it stops by itself after Goose is closed) / stop it now |
| `avery pull` | Download the model once (online, foreground; Ctrl+C when it says Ready) |
| `avery model` | Run the local model server in the foreground (no service) |
| `avery app` / `avery cli` | Start Goose Desktop / the Goose CLI with the Avery profile |
| `avery doctor` | Check prerequisites and setup |

The command line output is German for now.

## Configuration

`avery init` creates `~/.avery/avery.env` (a commented template). Known keys: `AVERY_MODEL`, `AVERY_MAX_MEMORY`,
`AVERY_MAX_CONTEXT`, `AVERY_MAX_CACHE_DISK`, `AVERY_OFFLINE`, `AVERY_IDLE_MINUTES`, `AVERY_GOOSE_APP`, `AVERY_GOOSE_ROOT`,
`AVERY_NO_ONBOARD` (1 = no setup dialogs when Avery starts). The file is parsed, not executed, and real environment variables win. The defaults are tuned for a 48 GB Mac.

The model runs offline by default (`AVERY_OFFLINE=1`): Avery never downloads anything on its own, so a service start
cannot pull a new multi-gigabyte model revision. `avery pull` is the one command that goes online.

| Path | Content |
|---|---|
| `~/.avery/goose` | Goose profile (`config/`, `top-of-mind.md`, sessions) |
| `~/.avery/mail/config.json` | The enabled mail accounts and calendars with their levels and the hourly limits (format version 4, written by `avery mail setup`, `revoke` and the setup dialogs of `avery mail onboard`) |
| `~/.avery/mail/config.lock` | Empty lock file: those commands hold it only while they re-read and save `config.json` (a few milliseconds, never while you answer prompts), so two terminals never overwrite each other; a command that cannot get it within 5 seconds stops and saves nothing |
| `~/.avery/mail/onboard.dismissed` | Empty marker file (owner-only), created when you choose *Nicht mehr fragen* in the first dialog: `avery mail onboard --if-needed` is silent while it exists. Delete it to be asked again; the config format is not involved |
| `~/.avery/mail/sendlog.jsonl` | Audit log of send and draft attempts (format version 1; no mail text, only a short hash) |
| `~/.avery/mail/changes.jsonl` | Log of calendar changes with the event before and after (format version 1; used by `avery mail undo`) |

Set `AVERY_HOME` to use another base folder. The lock file is created (owner-only) when it is missing and never written
afterwards: an existing one is only opened for reading, so a read-only lock file works and may stay as it is.

`config.json` is written by the commands above; it looks like this (you only edit the two limits by hand):

```json
{
  "version": 4,
  "mail": [{"id": "0C1E9B7A", "name": "iCloud", "type": "imap", "addresses": ["me@icloud.example"], "level": "read"}],
  "calendars": [{"id": "4F8A2D11", "name": "Privat", "level": "write"}],
  "sends_per_hour": 5,
  "changes_per_hour": 10
}
```

`mail` and `calendars` are optional lists (at most 200 entries each); the ids are the ones Mail and Calendar report.
Everything is enabled exactly when one of the lists has an entry. A mail or calendar entry without a `name` is shown under
its `id`, cut to one line of at most 120 characters. Unknown keys are rejected, and so is every other `version`: that file
is never rewritten (see [Update from 0.5.0](#update-from-050)). Every change the commands make is read back with the same
checks before it is saved; a file that would be refused is never written.

## Known limitations

- Everything about the real Mail.app and Calendar.app was built against their scripting interfaces and tested with mocks
  and demo data only. `avery mail selftest` and `selftest-write` are the first run against your accounts: look at the
  result.
- The setup dialogs were tested with a mock of the scripting interface and with scripted answers, not on a real screen:
  watch the first start once. If the dialogs do not show, `avery mail onboard` in a terminal says why, and
  `AVERY_NO_ONBOARD=1` with `avery mail setup` always works.
- macOS asks for permission to control Mail and Calendar for the app that starts the server (Terminal for the command
  line, Goose for the agent); if Goose does not get the prompt, use `avery cli session` from a terminal.
- Mail search covers subject and sender, not the message body.
- Single events without attendees can be changed; links on events (`url`) are not supported.
- The app and the cask are for Apple silicon and macOS 26 or later; the app is not notarized.
- The model server settings assume Splash and the model named in `avery.env`.

## Development

```sh
cargo test --manifest-path avery-mail/Cargo.toml   # mail and calendar server: unit and end-to-end tests on demo data, no Mail or Calendar access
tests/cli.sh                                        # bin/avery, app builder, disk image tools, cask and formula
MAIL_MCP_FAKE=1 cargo run --manifest-path avery-mail/Cargo.toml -- status   # demo data; setup, onboard and revoke refuse in demo mode
AVERY_MAIL_BIN=$PWD/avery-mail/target/debug/avery-mail bin/avery doctor   # AVERY_MAIL_BIN: use another mail server binary
AVERY_DRY_RUN=1 bin/avery model   # print the command instead of running it
```

The setup dialogs and the check for a graphical session run `/usr/bin/osascript` and `/bin/launchctl`. For
`avery mail onboard` and nothing else, `AVERY_OSASCRIPT` and `AVERY_LAUNCHCTL` (absolute paths) name other programs in
debug builds: the approval dialogs, the Apple bridge and the server always use the system ones and cannot reach this
override, and a release binary ignores it. Every binary-level test (`avery-mail/tests` and `tests/cli.sh`) points both at
stand-ins that record the call in a file and fail, and fails if a call was not expected, so a regression cannot open a real
dialog. A value that is not an absolute path leaves the dialogs dark instead of searching the `PATH`. Never run
`avery mail setup` or `avery mail onboard` in a test: they start the real Mail and Calendar.

### Logo, app and disk image

The artwork is vector: `tools/gen-logo.js` writes `assets/avery-icon.svg` (squircle with margin, for the README and the
volume icon), `assets/avery-app-icon.svg` (full-bleed square: macOS 26 applies its own mask and edge to it, a squircle with
margin would be put on a grey plate) and `assets/avery-mark.svg` (the "A" with the flap on a transparent background).

```sh
node tools/gen-logo.js          # SVG sources
tools/render-assets.sh          # share/avery/*.icns, assets/logo.png, assets/dmg/background.tiff (needs Edge, Chrome or Chromium)
tools/make-dmg.sh               # dist/Avery-<version>.dmg: app, Applications shortcut, background, volume icon (Finder lays it out)
tools/verify-dmg.sh             # mounts the image and checks contents, signature, icon, version and window layout
tools/publish-dmg.sh            # tag dmg-v<version> with the image as its only file; pins the revision in Casks/avery.rb
```

`Avery.app` is a small AppleScript launcher (`libexec/avery-make-app`); it calls `/opt/homebrew/bin/avery up`.
The cask fetches the image with git over SSH from the tag `dmg-v<version>`, whose commit contains only that file,
because a release download from a private repository would need a token. A published tag is never moved.
Homebrew clones the whole repository, so every published image ends up in every clone of the tap (about 3.5 MB per
release). If that matters, publish the images in a separate repository or, once the repository is public, as release
assets and point the cask `url` there.

Release: bump `AVERY_VERSION` in `bin/avery` and `version` in `avery-mail/Cargo.toml` (and `Cargo.lock`), commit, tag `v<version>`, run
`tools/make-dmg.sh`, `tools/verify-dmg.sh` and `tools/publish-dmg.sh`, then pin `revision` of the tag `v<version>` in
`Formula/avery.rb`, commit and push (`git push origin main v<version>`).

## License

MIT
