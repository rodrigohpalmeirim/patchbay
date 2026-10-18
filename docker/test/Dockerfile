# Throwaway OpenSSH servers for Patchbay integration tests. TEST ONLY - insecure on purpose.
# One image definition, three roles (target / bastion / sftponly) selected by ROLE.
FROM debian:bookworm-slim

ARG ROLE=target

RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server openssl ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && rm -f /etc/ssh/ssh_host_* \
 && mkdir -p /run/sshd \
 # pam_loginuid fails inside unprivileged containers and would abort every session.
 && sed -i 's/^\(session\s\+\)required\(\s\+pam_loginuid.so\)/\1optional\2/' /etc/pam.d/sshd

# Test user: patchbay / patchbay (uid 1000).
RUN useradd --create-home --uid 1000 --shell /bin/bash patchbay \
 && echo 'patchbay:patchbay' | chpasswd

COPY keys/id_ed25519.pub keys/id_ed25519_pass.pub keys/id_rsa.pub /tmp/keys/
RUN install -d -m 700 -o patchbay -g patchbay /home/patchbay/.ssh \
 && cat /tmp/keys/*.pub > /home/patchbay/.ssh/authorized_keys \
 && chown patchbay:patchbay /home/patchbay/.ssh/authorized_keys \
 && chmod 600 /home/patchbay/.ssh/authorized_keys \
 && rm -rf /tmp/keys

# Fixed host keys so known_hosts entries survive rebuilds.
COPY hostkeys/${ROLE}/ /etc/ssh/
RUN chmod 600 /etc/ssh/ssh_host_*_key && chmod 644 /etc/ssh/ssh_host_*_key.pub

COPY sshd_config.${ROLE} /etc/ssh/sshd_config
COPY setup-fixtures.sh /usr/local/sbin/setup-fixtures.sh
RUN sh /usr/local/sbin/setup-fixtures.sh "${ROLE}" && /usr/sbin/sshd -t

EXPOSE 22
HEALTHCHECK --interval=2s --timeout=3s --start-period=5s --retries=15 \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/22 && head -c 7 <&3 | grep -q SSH-2.0'
CMD ["/usr/sbin/sshd", "-D", "-e"]
