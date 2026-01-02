# Server Setup

## Create cama user

```bash
# Create user with home directory
sudo useradd -m -s /bin/bash cama

# Add to docker group (to run docker without sudo)
sudo usermod -aG docker cama
```

## Generate SSH deploy key

```bash
# On the server, as cama user
sudo -u cama ssh-keygen -t ed25519 -C "cama-deploy" -f /home/cama/.ssh/id_ed25519 -N ""

# Add public key to authorized_keys
sudo -u cama bash -c 'cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys'
sudo -u cama chmod 600 /home/cama/.ssh/authorized_keys

# Get the private key for GitHub secrets
sudo cat /home/cama/.ssh/id_ed25519
```

## Clone repo and setup

```bash
sudo -u cama bash -c '
cd ~
git clone https://github.com/cama-apps/cama-mm.git
cd cama-mm
mkdir -p data
'
```

## Create .env file

```bash
sudo -u cama nano /home/cama/cama-mm/.env
```

Add:
```
DISCORD_BOT_TOKEN=your_token_here
ADMIN_USER_IDS=123456789,987654321
```

## GitHub Secrets

Add these secrets to the repo (Settings → Secrets → Actions):

| Secret | Value |
|--------|-------|
| `SSH_HOST` | Your server IP/hostname |
| `SSH_KEY` | Contents of `/home/cama/.ssh/id_ed25519` (private key) |

## First deploy

```bash
sudo -u cama bash -c '
cd ~/cama-mm
docker compose build
docker compose up -d
'
```

## Logs

```bash
sudo -u cama docker compose -f /home/cama/cama-mm/docker-compose.yml logs -f
```
