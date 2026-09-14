# Security Policy

## Supported Versions

We take security seriously and provide security updates for the latest version zbrew.
We strongly recommend keeping your zbrew dependencies up to date.

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

If you discover a security vulnerability in zbrew or any of the related crates, please report it privately using
[GitHub's private security advisory feature](https://github.com/HernandoR/zbrew/security/advisories/new) on this repository.

Please include the following information in your report:

- A clear description of the vulnerability
- Steps to reproduce the issue
- Potential impact and attack scenarios
- Any suggested fixes or mitigations
- Your contact information for follow-up questions

### What constitutes a security vulnerability?

For zbrew, security vulnerabilities may include but are not limited to:

- **Package substitution**: Ability to install malicious packages by manipulating formula resolution or tap priority
- **Privilege escalation**: Unauthorized access to system directories or operations that should require elevated permissions
- **Credential exposure**: Leaking API tokens, tap credentials, or other sensitive data through logs, errors, or memory
- **Path traversal**: Malicious bottle archives that could write files outside the intended cellar/store directories
- **Denial of service**: Crashes, hangs, or resource exhaustion that prevent legitimate package management
- **Information disclosure**: Exposing installed packages, store paths, or system configuration to unauthorized processes
- **Input validation failures**: Improper handling of malformed formula names, tap references, or bottle data leading to undefined behavior
- **Race conditions**: Timing vulnerabilities in concurrent downloads, extraction, or linking that could lead to security issues
- **Checksum bypass**: Ability to install packages without proper SHA256 verification or with manipulated checksums
- **Symlink attacks**: Malicious symlinks in bottles that could overwrite system files during materialization or linking
- **Dependency confusion**: Installing malicious packages by exploiting tap resolution order or dependency specifications
- **Supply chain attacks**: Compromised taps or formula sources serving backdoored bottles
- **Arbitrary code execution**: Malicious bottles containing executables that run with user privileges during installation
- **Dependency vulnerabilities**: Security issues in upstream crates (reqwest, tokio, tar, etc.) that affect zbrew

For the CLI specifically:
- **Command injection**: Malicious formula names or tap references that could execute unintended shell commands
- **File system access**: Unauthorized reading or writing of files outside the intended root/prefix directories

## Response Timeline

This is a volunteer-maintained fork. We will triage security reports and work on fixes as soon as the maintainer can get to them. There is no guaranteed response time, but we will make our best effort to address valid security concerns.

## Disclosure Policy

We follow a coordinated disclosure process:

1. **Private disclosure**: We will work with you to understand and validate the vulnerability
2. **Fix development**: We will develop and test a fix in a private repository if necessary
3. **Coordinated release**: We will coordinate the public disclosure with the release of a fix
4. **Public disclosure**: After a fix is available, we will publish a security advisory

We request that you:
- Give us reasonable time to address the vulnerability before making it public
- Avoid accessing or modifying data beyond what is necessary to demonstrate the vulnerability
- Act in good faith and avoid privacy violations or destructive behavior

## Security Advisories

Published security advisories will be available through:

- GitHub Security Advisories on the
  [zbrew repository](https://github.com/HernandoR/zbrew/security/advisories)
- [RustSec Advisory Database](https://rustsec.org/)
- Release notes and changelog entries

## Recognition

We appreciate the security research community's efforts to improve the security of zbrew. With
your permission, we will acknowledge your contribution in:

- Security advisories
- Release notes
- Project documentation

If you prefer to remain anonymous, please let us know in your report.

## Scope

This security policy covers zbrew wholly.

## Additional Resources

- [Contributing Guidelines](CONTRIBUTING.md)
- [Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct)
- [Rust Security Policy](https://www.rust-lang.org/policies/security)

Thank you for helping to keep zbrew and the Rust ecosystem secure!
