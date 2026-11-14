cask "sodam" do
  version "0.3.9"
  sha256 "9d9d9ea235efbf0c78d4931bcbc13c149a78343133f3c10aa720a17587f5ef31"

  url "https://github.com/sodahub-org/sodam/releases/download/v#{version}/sodam-#{version}-macos-aarch64.zip"
  name "SodaM"
  desc "Native Qishui Music desktop client"
  homepage "https://github.com/sodahub-org/sodam"

  depends_on macos: :monterey
  depends_on arch: :arm64

  app "SodaM.app"

  zap trash: "~/Library/Application Support/sodam"
end
