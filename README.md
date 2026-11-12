# avery

A private AI agent for macOS: [Goose](https://github.com/block/goose) Desktop, a local model served by
[Splash](https://github.com/incoai/splash), and a small Apple Mail bridge that reads your private mail and can
write drafts or send mail, but only after you click **Senden** in a native macOS dialog.

Everything runs on your Mac. Mail credentials stay in macOS: Avery drives Mail.app and never sees passwords or tokens.

> Status: early (v0.1.0). After installing, run `avery mail selftest` and `avery mail selftest-write`.
> They check the Mail integration on your machine, which the automated tests cannot do.

## What is in the box

| Part | What it does |
|---|---|
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
brew install flavioaiello/avery/avery
```

If the repository is public, tap it with `https://github.com/flavioaiello/avery` instead.

## First run

```sh
avery init            # create the Goose profile in ~/.avery/goose
avery mail setup      # choose which Mail accounts the agent may read
avery mail selftest   # check read access (macOS asks once to let Terminal control Mail)
avery model           # start the local model; keep this terminal window open
avery app             # start Goose Desktop with the Avery profile
```

Writing is off until you enable it per account:

```sh
avery mail write-enable    # drafts and sending (with approval) for chosen accounts
avery mail selftest-write  # test the approval dialog, a draft, and optionally a mail to yourself
```

Restart Goose after changing accounts or permissions. `avery doctor` shows what is missing.

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
| `avery model` | Run the local model server in the foreground |
| `avery app` / `avery cli` | Start Goose Desktop / the Goose CLI with the Avery profile |
| `avery doctor` | Check prerequisites and setup |

The command line output is German for now.

## Configuration

`avery init` creates `~/.avery/avery.env` (a commented template). Known keys: `AVERY_MODEL`, `AVERY_MAX_MEMORY`,
`AVERY_MAX_CONTEXT`, `AVERY_MAX_CACHE_DISK`, `AVERY_OFFLINE`, `AVERY_GOOSE_APP`, `AVERY_GOOSE_ROOT`. The file is parsed,
not executed, and real environment variables win. The defaults are tuned for a 48 GB Mac.

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
- The model server settings assume Splash and the model named in `avery.env`.

## Development

```sh
python3 tests/test_mail.py        # about 110 checks against synthetic demo data, no Mail access
MAIL_MCP_FAKE=1 libexec/avery-mail status
AVERY_DRY_RUN=1 bin/avery model   # print the command instead of running it
```

## License

MIT
