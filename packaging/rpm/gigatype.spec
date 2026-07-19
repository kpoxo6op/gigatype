Name:           gigatype
Version:        0.3.0
Release:        1%{?dist}
Summary:        Private local Russian voice typing with GigaAM v3
License:        MIT
URL:            https://github.com/kpoxo6op/gigatype
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz
BuildRequires:  cargo rust cmake gcc-c++ pkgconfig(alsa) openssl-devel
Requires:       curl ca-certificates

%description
GigaType transcribes Russian speech locally with GigaAM v3 RNN-T and inserts
the result into the focused desktop application.

%prep
%autosetup

%build
cargo build --locked --release

%install
install -Dm755 target/release/gigatype %{buildroot}%{_bindir}/gigatype
install -Dm644 packaging/systemd/gigatype.service %{buildroot}%{_userunitdir}/gigatype.service

%files
%license LICENSE
%doc README.md
%{_bindir}/gigatype
%{_userunitdir}/gigatype.service

%changelog
* Sat Jul 18 2026 GigaType contributors <kpoxo6op@gmail.com> - 0.3.0-1
- Portable Unix runtime with native ONNX inference and CPAL audio
