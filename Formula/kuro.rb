# Homebrew formula for kuro.
#
# Until the first tagged release exists there is no source tarball to check a
# sha256 against, so this ships a `head` spec only and installs with:
#
#     brew install --HEAD suryakant/kuro/kuro
#
# At first release, add a stable stanza above `head`:
#
#     url "https://github.com/suryakant/kuro/archive/refs/tags/v0.1.0.tar.gz"
#     sha256 "<shasum -a 256 of that tarball>"
#
# and users can then `brew install suryakant/kuro/kuro` without --HEAD.
class Kuro < Formula
  desc "Terminal anime streaming client that plays in IINA"
  homepage "https://github.com/suryakant/kuro"
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
      kuro plays video through IINA, which is not installable as a formula:

        brew install --cask iina

      Run `kuro doctor` to check that everything is wired up.
    EOS
  end

  test do
    assert_match "kuro", shell_output("#{bin}/kuro --version")

    # Providers are compiled into the binary, so this works with no network
    # access and no config file present.
    assert_match "luciferdonghua", shell_output("#{bin}/kuro provider list")
  end
end
