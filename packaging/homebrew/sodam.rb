cask "sodam" do
  version "0.3.8"
  sha256 "52d0e7533786acc68e1070bf6b6bb89f4a6cfd78bdd4a6596edd9f91cf367db4"

  url "https://github.com/sodahub-org/sodam/releases/download/v#{version}/sodam-#{version}-macos-aarch64.zip"
  name "SodaM"
  desc "Native Qishui Music desktop client"
  homepage "https://github.com/sodahub-org/sodam"

  depends_on macos: :monterey
  depends_on arch: :arm64

  app "SodaM.app"

  zap trash: "~/Library/Application Support/sodam"
end
