<p align="center"><img src="assets/logo.png" width="128" alt="Avery"></p>

# avery

A private AI agent for macOS: [Goose](https://github.com/block/goose) Desktop, a local model served by
[Splash](https://github.com/incoai/splash), and a small Apple Mail bridge that reads your private mail and can
write drafts or send mail, but only after you click **Senden** in a native macOS dialog.

Everything runs on your Mac. Mail credentials stay in macOS: Avery drives Mail.app and never sees passwords or tokens.

> Status: early (v0.4.0). After installing, run `avery mail selftest` and `avery mail selftest-write`.
> They check the Mail integration on your machine, which the automated tests cannot do.

## What is in the box

| Part | What it does |
|---|---|
| `Avery.app` | The icon in Spotlight and the Dock: starts the model, waits until it is ready and opens Goose with the Avery profile |
| `avery` | Command line: creates the Goose profile, starts the model and Goose, checks the setup |
| `avery-mail` | MCP server for Goose (stdio, Python standard library only) that drives Mail.app through `osascript` |
| Goose profile | Config, permissions and a local provider for Splash, created by `avery init` in `~/.avery/goose`; your default Goose profile is not touched |

## Requirements

- Apple silicon Mac with macOS and Mail.app set up with your accounts
- [Homebrew](https://brew.sh), `/usr/bin/python3` (Xcode Command Line Tools)
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
avery init            # create the Goose profile in ~/.avery/goose
avery pull            # download the model once (about 17 GB); stop it with Ctrl+C when it says Ready
avery mail setup      # choose which Mail accounts the agent may read
avery mail selftest   # check read access (macOS asks once to let Terminal control Mail)
```

Writing is off until you enable it per account:

```sh
avery mail write-enable    # drafts and sending (with approval) for chosen accounts
avery mail selftest-write  # test the approval dialog, a draft, and optionally a mail to yourself
```

## Daily use

Open **Avery** with Spotlight (Cmd+Space) or drag it from the Applications folder to the Dock. It starts the model,
waits until it is ready and opens Goose with the Avery profile. If Goose is already open with that profile, it just
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

Restart Goose after changing accounts or permissions.

## How sending is protected

The agent reads untrusted mail, so the design assumes that a mail can try to talk the model into doing something.

- **Allow list.** Only accounts you chose with `avery mail setup` are ever touched.
- **Writing is opt-in** per account (`avery mail write-enable`), done by you in a terminal. The model cannot change it.
- **Approval dialog.** `send_message` builds the mail in Mail.app and reads the content back from it. A native dialog,
  shown by the server process and not by Goose, lists sender, recipients, subject and the exact text. Only a click on
  *Senden* sends. Cancel, a timeout (75 s) or any error means nothing is sent. The model cannot operate the dialog.
- **Goose asks too.** `permission.yaml` sets `mail__send_message` to `ask_before`.
- **Limits.** Plain text only, no Bcc, no attachments, at most 5 recipients, at most 1500 characters per sent mail
  (longer texts must be drafts you send from Mail), 5 sent mails per hour, 6 attempts per 10 minutes, a 20 second
  cool-down after you decline. Every attempt is logged (`avery mail sendlog`); if the log cannot be written, nothing is sent.
- **No destructive tools.** There is no code that deletes, moves or flags mail.
- **Replies** use Mail's reply: Mail picks the recipients and the subject.

The dialog is the hard gate. Instructions to the model (`top-of-mind.md`) are only a second line of defence.
The profile disables Goose's shell and file extensions; do not enable them for this profile.

## Commands

| Command | Purpose |
|---|---|
| `avery init [--force]` | Create the profile (existing files are kept; `--force` replaces them and keeps a backup) |
| `avery mail setup` / `revoke` | Choose or remove readable accounts (`revoke` also switches writing off) |
| `avery mail write-enable` / `write-disable` | Allow or forbid drafts and sending per account |
| `avery mail selftest` / `selftest-write` | Check read access / the approval dialog and drafts |
| `avery mail status` / `accounts` / `sendlog` | Show configuration, Mail accounts, recent send attempts |
| `avery install-app` | Create `Avery.app` in `~/Applications` without the cask (skipped when the cask's app is installed) |
| `avery up` / `avery down` | Start the model service and open Goose (it stops by itself after Goose is closed) / stop it now |
| `avery pull` | Download the model once (online, foreground; Ctrl+C when it says Ready) |
| `avery model` | Run the local model server in the foreground (no service) |
| `avery app` / `avery cli` | Start Goose Desktop / the Goose CLI with the Avery profile |
| `avery doctor` | Check prerequisites and setup |

The command line output is German for now.

## Configuration

`avery init` creates `~/.avery/avery.env` (a commented template). Known keys: `AVERY_MODEL`, `AVERY_MAX_MEMORY`,
`AVERY_MAX_CONTEXT`, `AVERY_MAX_CACHE_DISK`, `AVERY_OFFLINE`, `AVERY_IDLE_MINUTES`, `AVERY_GOOSE_APP`, `AVERY_GOOSE_ROOT`. The file is parsed,
not executed, and real environment variables win. The defaults are tuned for a 48 GB Mac.

The model runs offline by default (`AVERY_OFFLINE=1`): Avery never downloads anything on its own, so a service start
cannot pull a new multi-gigabyte model revision. `avery pull` is the one command that goes online.

| Path | Content |
|---|---|
| `~/.avery/goose` | Goose profile (`config/`, `top-of-mind.md`, sessions) |
| `~/.avery/mail/config.json` | Allow list and write settings (created by `avery mail setup`) |
| `~/.avery/mail/sendlog.jsonl` | Audit log of send attempts (no mail text, only a short hash) |

Set `AVERY_HOME` to use another base folder.

## Known limitations

- macOS asks for permission to control Mail for the app that starts the server (Terminal for the CLI, Goose for the agent).
  If Goose does not get the prompt, use `avery cli session` from a terminal.
- Mail search covers subject and sender, not the message body.
- The app and the cask are for Apple silicon and macOS 26 or later; the app is not notarized.
- The model server settings assume Splash and the model named in `avery.env`.

## Development

```sh
python3 tests/test_mail.py        # about 185 checks against synthetic demo data, no Mail access
MAIL_MCP_FAKE=1 libexec/avery-mail status
AVERY_DRY_RUN=1 bin/avery model   # print the command instead of running it
```

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

Release: bump `AVERY_VERSION` in `bin/avery` and `VERSION` in `libexec/avery-mail`, commit, tag `v<version>`, run
`tools/make-dmg.sh`, `tools/verify-dmg.sh` and `tools/publish-dmg.sh`, then pin `revision` of the tag `v<version>` in
`Formula/avery.rb`, commit and push (`git push origin main v<version>`).

## License

MIT
