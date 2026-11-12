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
  desc "Launcher for the private AI agent Avery (Goose, local model, Apple Mail)"
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
      avery mail setup
  EOS
end
