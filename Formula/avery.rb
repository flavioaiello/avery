class Avery < Formula
  desc "Private AI agent for macOS: Goose, a local model and a Gmail and Outlook.com bridge"
  homepage "https://github.com/flavioaiello/avery"
  url "git@github.com:flavioaiello/avery.git",
      using:    :git,
      tag:      "v0.4.0",
      revision: "888f7e03cfa8bddadaa0221d15039b193c59bcd2"
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

      Then open "Avery" (Spotlight, the Dock, or: avery up). While no account is linked, the first start guides you
      through the setup in macOS dialogs: Apple Mail and Calendar (the quickest start: your accounts from Mail.app, no
      registration) and, optionally, Gmail and Outlook.com directly (personal accounts; you register your own OAuth app
      once, see the README). macOS may ask for permission to show notifications;
      only the optional Apple bridge asks once for Automation (Mail and Calendar): allow what Avery should use. A failed setup is also shown as a notification;
      if no dialog and no notification appears, run "avery mail onboard" in a terminal to see the reason.
      Cancelling a dialog saves nothing more; Avery starts anyway.

      The same from a terminal:
        avery mail onboard         # shows the dialogs again
        avery mail link google     # or: microsoft
        avery mail setup           # per account and calendar: off, read or write
        avery mail selftest
      No dialogs at start: choose "Nicht mehr fragen" in the first one, or set AVERY_NO_ONBOARD=1 (in ~/.avery/avery.env).

      Apple Mail and Calendar are optional and off until you choose them in the dialogs or run: avery mail setup apple

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
