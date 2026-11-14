class Avery < Formula
  desc "Private AI agent for macOS: Goose, a local model, Apple Mail and Calendar bridge"
  homepage "https://github.com/flavioaiello/avery"
  url "git@github.com:flavioaiello/avery.git",
      using:    :git,
      tag:      "v0.5.0",
      revision: "c4d04d5ba321a50e32c5d88decd6ffc33a201f73"
  license "MIT"
  head "git@github.com:flavioaiello/avery.git", using: :git, branch: "main"

  depends_on arch: :arm64
  depends_on "incoai/tap/splash"
  depends_on "rust" => :build
  depends_on :macos

  def install
    system "cargo", "install", *std_cargo_args(path: "avery-mail")
    bin.install "bin/avery"
    libexec.install "libexec/avery-make-app"
    pkgshare.install Dir["share/avery/*"]
  end

  service do
    run [opt_bin/"avery", "model"]
    keep_alive crashed: true
    log_path var/"log/avery-model.log"
    error_log_path var/"log/avery-model.log"
    environment_variables PATH: std_service_path_env
  end

  def caveats
    <<~EOS
      Goose Desktop is a cask and is not installed by this formula:
        brew install --cask block-goose

      The app (Avery.app with its own icon, in /Applications) is a cask from the same tap:
        brew install --cask flavioaiello/avery/avery

      First run:
        avery init
        avery pull     # downloads the model once (about 17 GB); Ctrl+C when it says Ready

      Then open "Avery" (Spotlight, the Dock, or: avery up). While nothing is set up, the first start guides you
      through the setup in macOS dialogs: Avery lists the accounts and calendars of Apple Mail and Calendar and you
      choose what it may read or write. macOS may ask for permission to show notifications; macOS asks once for
      Automation (Mail and Calendar): allow what Avery should use. A failed setup is also shown as a notification;
      if no dialog and no notification appears, run "avery mail onboard" in a terminal to see the reason.
      Cancelling a dialog saves nothing more; Avery starts anyway.

      The same from a terminal:
        avery mail onboard         # shows the dialogs again
        avery mail setup           # per account and calendar: off, read or write
        avery mail selftest
      No dialogs at start: choose "Nicht mehr fragen" in the first one, or set AVERY_NO_ONBOARD=1 (in ~/.avery/avery.env).

      Upgrading from 0.5.0: it read config format 3 and could link Gmail and Outlook.com directly. Version 0.6.0 reads
      only format 4 and uses only Apple Mail and Calendar: move ~/.avery/mail/config.json away and run avery mail setup.
      The sign-ins that 0.5.0 stored in the Keychain stay there until you remove them (see the README, "Update from 0.5.0").

      Daily use: open "Avery" with Spotlight or the Dock.
        It starts the model, opens Goose, and stops the model by itself a few minutes after Goose is closed.
        The same from a terminal: avery up   (avery down stops the model right away)
        Without the cask, "avery install-app" creates the app in ~/Applications.

      The model service holds up to 28 GB of memory while it runs (AVERY_MAX_MEMORY).
      To start it at every login instead: brew services start avery
    EOS
  end

  test do
    ENV["HOME"] = testpath
    assert_match "avery #{version}", shell_output("#{bin}/avery version")

    system bin/"avery", "init"
    assert_match "avery-mail", (testpath/".avery/goose/config/config.yaml").read
    assert_path_exists testpath/".avery/goose/top-of-mind.md"

    ENV["MAIL_MCP_FAKE"] = "1"
    requests = <<~JSON
      {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}
      {"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
    JSON
    output = pipe_output("#{bin}/avery-mail serve", requests)
    assert_match "list_messages", output
    assert_match "list_events", output
    refute_match "send_message", output
    refute_match "create_event", output
  end
end
