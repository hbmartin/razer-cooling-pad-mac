# Homebrew formula template for padctl.
#
# To publish: create a tap repository (github.com/hbmartin/homebrew-tap),
# copy this file into its Formula/ directory, and fill in sha256 for the
# tagged tarball:
#
#   curl -sL https://github.com/hbmartin/razer-cooling-pad-mac/archive/refs/tags/v0.2.0.tar.gz | shasum -a 256
#
# Users then install with:
#
#   brew tap hbmartin/tap
#   brew install padctl
class Padctl < Formula
  desc "Control the fans and lights of a Razer Laptop Cooling Pad"
  homepage "https://github.com/hbmartin/razer-cooling-pad-mac"
  url "https://github.com/hbmartin/razer-cooling-pad-mac/archive/refs/tags/v0.2.0.tar.gz"
  sha256 "" # fill in per release, see header comment
  license "MIT"
  head "https://github.com/hbmartin/razer-cooling-pad-mac.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    generate_completions_from_executable(bin/"padctl", "completions")
    (buildpath/"padctl.1").write Utils.safe_popen_read(bin/"padctl", "manpage")
    man1.install "padctl.1"
  end

  test do
    assert_match "padctl", shell_output("#{bin}/padctl --version")
  end
end
