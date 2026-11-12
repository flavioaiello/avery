class Avery < Formula
  desc "Private AI agent for macOS: Goose, a local model and an Apple Mail bridge"
  homepage "https://github.com/flavioaiello/avery"
  url "git@github.com:flavioaiello/avery.git",
      using:    :git,
      tag:      "v0.1.0",
      revision: "0000000000000000000000000000000000000000"
  license "MIT"
  head "git@github.com:flavioaiello/avery.git", using: :git, branch: "main"

  depends_on arch: :arm64
  depends_on "incoai/tap/splash"
  depends_on :macos

  def install
    bin.install "bin/avery"
    libexec.install "libexec/avery-mail"
    pkgshare.install Dir["share/avery/*"]
    bin.install_symlink libexec/"avery-mail"
  end

  def caveats
    <<~EOS
      Goose Desktop is a cask and is not installed by this formula:
        brew install --cask block-goose

      First run:
        avery init
        avery mail setup
        avery mail selftest
        avery model   # keep this terminal window open
        avery app
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
