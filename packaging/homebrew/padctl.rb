# Homebrew formula template for padctl.
#
# The release workflow (.github/workflows/release.yml) publishes this
# automatically: on a v* tag it fills in the tagged tarball's url + sha256
# and pushes the result to github.com/hbmartin/homebrew-tap as
# Formula/padctl.rb (requires the HOMEBREW_TAP_TOKEN secret). The tap
# repository just needs to exist.
#
# Users then install with:
#
#   brew tap hbmartin/tap
#   brew install padctl
class Padctl < Formula
  desc "Control the fans and lights of a Razer Laptop Cooling Pad"
  homepage "https://github.com/hbmartin/razer-cooling-pad-mac"
  url "https://github.com/hbmartin/razer-cooling-pad-mac/archive/refs/tags/v0.4.0.tar.gz"
  sha256 "" # filled in per release by the release workflow
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
