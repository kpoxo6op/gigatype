class Gigatype < Formula
  desc "Private local Russian voice typing with GigaAM v3"
  homepage "https://github.com/kpoxo6op/gigatype"
  url "https://github.com/kpoxo6op/gigatype/archive/refs/tags/v0.3.0.tar.gz"
  license "MIT"
  head "https://github.com/kpoxo6op/gigatype.git", branch: "main"

  depends_on "cmake" => :build
  depends_on "pkg-config" => :build
  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
    pkgshare.install "scripts/bootstrap-model.sh", "packaging/model-sha256.txt"
  end

  service do
    run [opt_bin/"gigatype", "daemon"]
    keep_alive true
    log_path var/"log/gigatype.log"
    error_log_path var/"log/gigatype.log"
  end

  def caveats
    "Run #{opt_bin}/gigatype doctor, then grant Microphone and Accessibility access when macOS asks."
  end

  test do
    assert_match "platform: macos", shell_output("#{bin}/gigatype platform")
  end
end
