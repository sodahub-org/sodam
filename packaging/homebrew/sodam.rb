cask "sodam" do
  version "0.4.1"
  sha256 "d8a01758df81b047a5b4e4661bc871500e288fd2e51ba8c5f2ad9173ee61dfe2"

  url "https://github.com/sodahub-org/sodam/releases/download/v#{version}/sodam-#{version}-macos-aarch64.zip"
  name "SodaM"
  desc "Native Qishui Music desktop client"
  homepage "https://github.com/sodahub-org/sodam"

  depends_on macos: :monterey
  depends_on arch: :arm64

  app "SodaM.app"

  zap trash: "~/Library/Application Support/sodam"
end
