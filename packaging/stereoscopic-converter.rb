# Homebrew cask. Copy into a tap repository named perkyPatLayouts/homebrew-tap
# as Casks/stereoscopic-converter.rb, then users install with:
#
#   brew install --cask --no-quarantine perkypatlayouts/tap/stereoscopic-converter
#
# --no-quarantine is what makes this route pleasant: the app is ad-hoc signed
# rather than notarised, so a quarantined copy is refused by Gatekeeper with a
# message claiming the app is damaged. Never setting the bit avoids that.
#
# `build-app.py --dmg` prints the version and sha256 to paste in below.
cask "stereoscopic-converter" do
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/perkyPatLayouts/Ana-Convert/releases/download/v#{version}/StereoscopicConverter-#{version}.dmg"
  name "Stereoscopic Converter"
  desc "Recovers full-colour stereo video from anaglyph 3D"
  homepage "https://github.com/perkyPatLayouts/Ana-Convert"

  # Every bundled binary is arm64. On an Intel Mac the app does not start.
  depends_on arch: :arm64
  depends_on macos: ">= :big_sur"

  app "Stereoscopic Converter.app"

  caveats <<~EOS
    Stereoscopic Converter is ad-hoc signed, not notarised through the Apple
    Developer Program. If you installed without --no-quarantine and macOS says
    the app is damaged, that is Gatekeeper declining to check an unnotarised
    signature. Clear the quarantine flag with:

      xattr -dr com.apple.quarantine "/Applications/Stereoscopic Converter.app"

    The app carries its own ffmpeg, so nothing else needs installing.
  EOS
end
