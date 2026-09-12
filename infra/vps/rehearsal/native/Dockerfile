# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# The test bed for the native path: stock Ubuntu 24.04 running real systemd as PID 1, so the units
# under test are loaded, ordered, sandboxed and cgroup-limited by the same code that will do it on
# the VPS. Nothing here is a simulation of systemd.
#
# WHERE THIS DIFFERS FROM A REAL VM, stated up front because a rehearsal that hides its own gaps is
# worse than no rehearsal (infra/vps/README.md makes the same admission for the Docker path's
# `internal: true` check):
#
#   1. The container runs --privileged with the host's cgroup tree mounted rw. That is what lets
#      systemd create per-unit cgroups at all. It also means CAP_SYS_ADMIN is present in the
#      container's bounding set, so a directive whose whole job is to REMOVE a capability
#      (CapabilityBoundingSet=) is proven to be accepted and applied, not proven to be the only
#      thing standing between an agent and the kernel. On a real VM the service user has no
#      capabilities to begin with.
#   2. MemoryMax/TasksMax/CPUQuota are enforced by the SAME kernel cgroup-v2 controllers as on a
#      VM, and the probes measure them for real. But the memory ceiling the OOM killer sees is the
#      container's, nested under the host's — so absolute numbers differ from a 3.8 GB Linode even
#      though the containment BEHAVIOUR (who gets killed, what systemd does next) is identical.
#   3. The kernel is the Docker host's (on a Mac, a Linux VM's), not Ubuntu 24.04's. Seccomp and
#      LSM behaviour can differ in principle. Every check here records what it actually observed
#      rather than asserting a kernel version.
#   4. No cloud firewall, no ufw (install.sh --firewall is not exercised here; nothing in a
#      container can prove a VM's packet filter).
#
# What it DOES prove, and what nothing short of a VM proved before: that the shipped unit files
# load, that the sandbox directives take effect, and that an agent can still clone a repo, install
# a dependency and run a build underneath them.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# systemd itself, plus exactly the things a minimal Ubuntu cloud image already has that install.sh
# assumes rather than installs (git is the documented manual prerequisite; the rest is base). The
# toolchain install.sh owns -- Node, the CLIs, Rust -- is deliberately NOT here: letting the image
# preinstall it would hide an installer that fails to install it.
RUN apt-get update -q \
    && apt-get install -y -q --no-install-recommends \
        systemd systemd-sysv dbus dbus-user-session \
        ca-certificates curl git sudo procps iproute2 less \
    && apt-get clean && rm -rf /var/lib/apt/lists/*

# A container has no getty, no login, and nothing to do on a console. Masking these keeps the boot
# quiet and stops systemd waiting on devices that do not exist.
RUN systemctl mask \
        systemd-udevd.service systemd-udevd-kernel.socket systemd-udevd-control.socket \
        systemd-modules-load.service sys-kernel-debug.mount sys-kernel-config.mount \
        getty.target console-getty.service getty-static.service \
        systemd-firstboot.service

# install.sh refuses to run anywhere but Ubuntu 24.04, by design. The image IS Ubuntu 24.04, so
# this is a tripwire on the base image changing under us, not a workaround.
RUN . /etc/os-release && [ "$ID" = ubuntu ] && [ "$VERSION_ID" = 24.04 ]

STOPSIGNAL SIGRTMIN+3
CMD ["/lib/systemd/systemd"]
