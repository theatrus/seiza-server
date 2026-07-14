%{!?_unitdir:%global _unitdir %{_prefix}/lib/systemd/system}
%undefine _debugsource_packages

Name:           seiza-server
Version:         %{?package_version}%{!?package_version:0.1.0}
Release:         %{?package_release}%{!?package_release:1}%{?dist}
Summary:         Queued Seiza plate-solving web service
License:         Apache-2.0
URL:             https://github.com/theatrus/seiza-server
Source0:         %{name}-%{version}.tar.gz
Provides:        user(seiza-server)
Provides:        group(seiza-server)

BuildRequires:   /usr/bin/cargo
BuildRequires:   /usr/bin/npm
BuildRequires:   /usr/bin/rpmbuild
Requires:        systemd
Requires(pre):   shadow-utils

%description
Seiza Server provides a durable queued plate-solving API, an Astrometry.net
compatible API subset, and a bundled React web interface. This package bundles
the server binary with AWS S3, SQS, and DynamoDB adapters enabled.

%prep
%autosetup -n %{name}-%{version}

%build
npm ci --prefix frontend
npm run build --prefix frontend
# aws-lc-sys runs an x86 compiler probe that intentionally ignores CFLAGS but
# retains LDFLAGS. Distro hardening specs force that probe to link as PIE, so
# supply the matching compile flag while preserving every RPM linker flag.
LDFLAGS="${LDFLAGS:-} -fPIE" cargo build --locked --release --features aws

%check
# Reuse the release dependency graph produced in %%build. A default-profile
# test would compile aws-lc-sys and the rest of the dependency graph again.
LDFLAGS="${LDFLAGS:-} -fPIE" cargo test --locked --release --features aws

%install
install -Dpm 0755 "${CARGO_TARGET_DIR:-target}/release/seiza-server" %{buildroot}%{_bindir}/seiza-server
install -d %{buildroot}%{_libexecdir}/seiza-server
cp -a frontend/dist %{buildroot}%{_libexecdir}/seiza-server/frontend
install -Dpm 0644 packaging/rpm/seiza-server.service %{buildroot}%{_unitdir}/seiza-server.service
install -Dpm 0644 packaging/rpm/seiza-server.env %{buildroot}%{_sysconfdir}/seiza-server/seiza-server.env
install -Dpm 0644 packaging/nginx/seiza-server.conf.example %{buildroot}%{_docdir}/%{name}/nginx.conf.example

%pre
if ! getent group seiza-server >/dev/null 2>&1; then
    groupadd --system seiza-server
fi
if ! getent passwd seiza-server >/dev/null 2>&1; then
    useradd --system --gid seiza-server --home-dir /var/lib/seiza-server \
        --shell /sbin/nologin --comment "Seiza Server service account" seiza-server
fi

%post
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || :
fi

%preun
if [ "$1" -eq 0 ] && command -v systemctl >/dev/null 2>&1; then
    systemctl --no-reload disable seiza-server.service >/dev/null 2>&1 || :
    systemctl stop seiza-server.service >/dev/null 2>&1 || :
fi

%postun
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || :
fi

%files
%license LICENSE
%doc README.md CHANGELOG.md docs/architecture.md docs/production-rpm.md
%doc %{_docdir}/%{name}/nginx.conf.example
%{_bindir}/seiza-server
%{_libexecdir}/seiza-server
%{_unitdir}/seiza-server.service
%dir %{_sysconfdir}/seiza-server
%attr(0640,root,seiza-server) %config(noreplace) %{_sysconfdir}/seiza-server/seiza-server.env

%changelog
* Mon Jul 13 2026 The Seiza Server Contributors <github@theatr.us> - 0.1.0-1
- Initial production RPM package
