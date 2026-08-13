# Homebrew formula for kuro.
#
# Lives in the tap repo `suryakant/homebrew-tap`, which Homebrew refers to as
# `suryakant/tap`. Install with either:
#
#     brew tap suryakant/tap && brew install kuro      # then just `kuro`
#     brew install suryakant/tap/kuro                  # one-liner
#
# The stable spec pins a git tag plus its revision rather than a release tarball,
# so no sha256 is needed and no GitHub release has to exist. When cutting a new
# version, bump both `tag` and `revision` together.
class Kuro < Formula
  desc "Terminal anime streaming client that plays in IINA"
  homepage "https://github.com/suryakant/kuro"
  url "https://github.com/suryakant/kuro.git",
      tag:      "v0.1.0",
      revision: "26b68933575a30d31f41628e5dec39df6d2e1965"
  license "MIT"
  head "https://github.com/suryakant/kuro.git", branch: "main"

  depends_on "rust" => :build
  depends_on :macos
  depends_on "yt-dlp"

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/kuro-cli")

    generate_completions_from_executable(bin/"kuro", "completions", shell_parameter_format: :none)
  end

  def caveats
    <<~EOS
      kuro plays video through IINA, which is a cask rather than a formula:

        brew install --cask iina

      Run `kuro doctor` to check that everything is wired up.
    EOS
  end

  test do
    assert_match "kuro", shell_output("#{bin}/kuro --version")

    # Providers are compiled into the binary, so this needs no network access
    # and no config file.
    assert_match "luciferdonghua", shell_output("#{bin}/kuro provider list")
  end
end
