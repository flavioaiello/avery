class Avery < Formula
  desc "Private AI agent for macOS: Goose, a local model and an Apple Mail bridge"
  homepage "https://github.com/flavioaiello/avery"
  url "git@github.com:flavioaiello/avery.git",
      using:    :git,
      tag:      "v0.3.0",
      revision: "5b77503a7041143794ec012cc429bb5f7e72a4e6"
  license "MIT"
  head "git@github.com:flavioaiello/avery.git", using: :git, branch: "main"

  depends_on arch: :arm64
  depends_on "incoai/tap/splash"
  depends_on :macos

  def install
    bin.install "bin/avery"
    libexec.install "libexec/avery-mail", "libexec/avery-make-app"
    pkgshare.install Dir["share/avery/*"]
    bin.install_symlink libexec/"avery-mail"
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
        avery mail setup
        avery mail selftest

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
    refute_match "send_message", output
  end
end
