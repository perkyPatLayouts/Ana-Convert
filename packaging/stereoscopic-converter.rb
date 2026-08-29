# Homebrew cask. Copy into a tap repository named perkyPatLayouts/homebrew-tap
# as Casks/stereoscopic-converter.rb, then users install with:
#
#   brew install --cask --no-quarantine perkypatlayouts/tap/stereoscopic-converter
#
# --no-quarantine is what makes this route pleasant: the app is ad-hoc signed
# rather than notarised, so a quarantined copy is refused by Gatekeeper with a
# message claiming the app is damaged. Never setting the bit avoids that.
#
#~ Lines marked `#~` are notes to whoever edits this template. They are dropped
#~ from the copy `build-app.py --dmg` generates, so they never reach the tap.
#~
#~ This file is a TEMPLATE. The sha256 below is a placeholder and matches
#~ nothing — do not publish this copy.
#~
#~ `build-app.py --dmg` writes the real one to `target/stereoscopic-converter.rb`
#~ with the version and digest of the image it just built already substituted.
#~ That is the file to copy into the tap. Filling the digest in by hand is how a
#~ cask ends up pinning the wrong bytes.
cask "stereoscopic-converter" do
  version "0.10.0"
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
