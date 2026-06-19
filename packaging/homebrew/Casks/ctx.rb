cask "ctx" do
  version "0.69.6"

  on_arm do
    sha256 "2f5de0f46c25dea5560e0eef087d8932b46296e6f1f0f3900aa4c42a1ed61a40"

    url "https://api.ctx.rs/functions/v1/download/stable/#{version}/ctx_#{version}_macos-arm64.dmg"
  end
  on_intel do
    sha256 "9ca796bedd3912ab911214aee05b7c13670b949d0393469824646b2f89f4fd94"

    url "https://api.ctx.rs/functions/v1/download/stable/#{version}/ctx_#{version}_macos-x64.dmg"
  end

  name "ctx"
  desc "Agentic Development Environment for coding agents"
  homepage "https://ctx.rs/"

  livecheck do
    url "https://api.ctx.rs/functions/v1/releases/stable/latest.json"
    strategy :json do |json|
      json["latest_version"]
    end
  end

  auto_updates true
  depends_on macos: :big_sur

  app "ctx.app"

  zap trash: [
    "~/.ctx",
    "~/Library/Application Support/rs.ctx.desktop",
    "~/Library/Caches/rs.ctx.desktop",
    "~/Library/HTTPStorages/rs.ctx.desktop",
    "~/Library/Preferences/rs.ctx.desktop.plist",
    "~/Library/Saved Application State/rs.ctx.desktop.savedState",
    "~/Library/WebKit/rs.ctx.desktop",
  ]
end
