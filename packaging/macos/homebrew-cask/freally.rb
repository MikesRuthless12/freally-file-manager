# Homebrew cask for Freally File Manager v1.0.0.
#
# Lives in a dedicated `freally-homebrew` tap (free to create and
# host on GitHub). Users install with:
#   brew tap freally/freally
#   brew install --cask freally
#
# Homebrew auto-clears the Gatekeeper quarantine flag on cask
# installs, so the ad-hoc-signed `.app` bundle runs on first launch
# without the "unidentified developer" warning. `sha256` is filled
# in at publish time by the release helper.
cask "freally" do
  version "1.0.0"
  sha256 arm:   "0000000000000000000000000000000000000000000000000000000000000000",
         intel: "0000000000000000000000000000000000000000000000000000000000000000"

  on_arm do
    url "https://github.com/MikesRuthless12/freally-file-manager/releases/download/v#{version}/Freally_#{version}_aarch64.dmg"
  end
  on_intel do
    url "https://github.com/MikesRuthless12/freally-file-manager/releases/download/v#{version}/Freally_#{version}_x64.dmg"
  end

  name "Freally File Manager v1.0.0"
  desc "Byte-exact cross-platform file copier (Rust + Tauri 2)"
  homepage "https://github.com/MikesRuthless12/freally-file-manager"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: ">= :monterey"

  app "Freally File Manager v1.0.0.app"

  zap trash: [
    "~/Library/Application Support/freally-file-manager",
    "~/Library/Preferences/com.freally.filemanager.plist",
    "~/Library/Caches/com.freally.filemanager",
  ]
end
