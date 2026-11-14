cask "avery" do
  version "0.4.0"
  sha256 :no_check # the git revision below pins the disk image

  # The disk image sits alone in the commit of the tag dmg-v<version> (see tools/publish-dmg.sh).
  # Git over SSH also works for a private repository, where release downloads would need a token.
  # A cask url must be a real URI, so ssh:// and not the scp form git@github.com:owner/repo.git.
  url "ssh://git@github.com/flavioaiello/avery.git",
      using:    :git,
      tag:      "dmg-v#{version}",
      revision: "16f3579a9ae2d14fd9e955732ab328c071bd7452"
  name "Avery"
  desc "Launcher for the private AI agent Avery (Goose, local model, Gmail and Outlook.com)"
  homepage "https://github.com/flavioaiello/avery"

  depends_on formula: "flavioaiello/avery/avery"
  depends_on macos: :tahoe

  container nested: "Avery-#{version}.dmg"

  app "Avery.app"

  # Signed ad hoc, not notarized: with the quarantine flag Gatekeeper would block the first start.
  postflight_steps do
    run "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "{{appdir}}/Avery.app"], must_succeed: false
  end

  uninstall quit: "io.github.flavioaiello.avery"

  caveats <<~EOS
    Avery.app starts the model and opens Goose with the Avery profile. It needs Goose Desktop:
      brew install --cask block-goose

    First run:
      avery init
      avery pull          # downloads the model once (about 17 GB); Ctrl+C when it says Ready

    Then open Avery. While no account is linked, its first start guides you through the setup in macOS dialogs:
    Apple Mail and Calendar (the quickest start: your accounts from Mail.app, no registration) and, optionally,
    Gmail and Outlook.com directly (personal accounts; you register your own OAuth app once, see the README). macOS may ask for permission to show notifications; only the optional Apple bridge
    asks once for Automation (Mail and Calendar): allow what Avery should use. A failed setup is also shown as a notification; if no dialog and no notification appears,
    run "avery mail onboard" in a terminal to see the reason. Cancelling any dialog saves nothing more, and Avery still starts.

    The same from a terminal:
      avery mail onboard         # shows the dialogs again
      avery mail link google     # or: microsoft
      avery mail setup           # per account and calendar: off, read or write
    No dialogs at start: choose "Nicht mehr fragen" in the first one, or set AVERY_NO_ONBOARD=1 (in ~/.avery/avery.env).
  EOS
end
