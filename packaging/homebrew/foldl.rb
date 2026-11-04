# Homebrew formula for foldl. Not in a tap yet: install from a checkout with
# `brew install --build-from-source ./packaging/homebrew/foldl.rb`.
# `url` and `sha256` name the latest release's source tarball; the checksum
# can only be filled in once its tag exists (see docs/releasing.md).
class Foldl < Formula
  desc "Run code when a shell enters or leaves a folder"
  homepage "https://github.com/skogsbrus/foldl"
  url "https://github.com/skogsbrus/foldl/archive/refs/tags/v0.1.1.tar.gz"
  sha256 "9b595a2470e95d21ca6615f25c51b61bbcaced8262ff6348674a2edf6a4df04e"
  license "MIT"
  head "https://github.com/skogsbrus/foldl.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/foldl")
    # The crates compiled into the binary require their notices with it.
    doc.install "THIRD_PARTY_LICENSES.md"
  end

  def caveats
    <<~EOS
      Add the hook to your shell:
        zsh:  echo 'eval "$(foldl hook zsh)"' >> ~/.zshrc
        bash: echo 'eval "$(foldl hook bash)"' >> ~/.bashrc
    EOS
  end

  test do
    assert_match "foldl", shell_output("#{bin}/foldl --version")
    (testpath/".foldl/1-env/manifest.toml").write "version = 1\n[env]\nGREETING = \"hi\"\n"
    system bin/"foldl", "allow", testpath
    assert_equal "hi\n", shell_output("#{bin}/foldl exec #{testpath} -- sh -c 'echo $GREETING'")
  end
end
