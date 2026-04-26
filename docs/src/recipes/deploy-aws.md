# Deploy on AWS (EC2)

Recipe for a single-AZ AWS deploy on `t4g.small` (ARM Graviton).
Fits a personal-agent or small team; production multi-AZ scale-out
needs Phase 32 multi-host orchestration.

## What you end up with

- Nexo daemon under systemd on EC2 + EBS gp3 for state
- Nginx + ACM cert for TLS termination (free)
- Route53 hostname pointing at the instance
- IAM role granting **only** SES send + S3 backup-bucket access
  (no console / no read of other AWS resources)
- Daily snapshot of the EBS volume + lifecycle policy retaining 30
- CloudWatch agent shipping `/var/log/nexo-rs/*.log` + metrics

Estimated cost (us-east-1, on-demand):
- `t4g.small` instance: **~$13.43/mo**
- `gp3` 16 GB EBS: **~$1.28/mo**
- Route53 hosted zone: **$0.50/mo**
- ACM cert: **free**
- SES outbound (5k emails/mo on free tier first 12 months):
  **free** then **$0.10/1k**
- Total: **~$15-20/mo**

Cheaper alternative for personal-agent budgets: use Hetzner's
CX22 at €4/mo if you don't need AWS-specific integrations.

## 0. Prerequisites

- AWS account with billing alarms set
- Route53 hosted zone for your domain
- AWS CLI installed and `aws configure`'d locally
- Terraform 1.5+ if you want infra-as-code (recommended)

## 1. Provision via Terraform (recommended)

The repo will eventually ship `deploy/terraform/aws/` (Phase 40
follow-up). Until then, here's a minimal `main.tf`:

```hcl
terraform {
  required_providers {
    aws = { source = "hashicorp/aws", version = "~> 5.0" }
  }
}

provider "aws" {
  region = "us-east-1"
}

# --- VPC + subnet -----------------------------------------------------
resource "aws_vpc" "nexo" {
  cidr_block           = "10.0.0.0/16"
  enable_dns_support   = true
  enable_dns_hostnames = true
  tags = { Name = "nexo" }
}

resource "aws_subnet" "nexo_public" {
  vpc_id                  = aws_vpc.nexo.id
  cidr_block              = "10.0.1.0/24"
  availability_zone       = "us-east-1a"
  map_public_ip_on_launch = true
}

resource "aws_internet_gateway" "nexo" {
  vpc_id = aws_vpc.nexo.id
}

resource "aws_route_table" "nexo_public" {
  vpc_id = aws_vpc.nexo.id
  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.nexo.id
  }
}

resource "aws_route_table_association" "nexo_public" {
  subnet_id      = aws_subnet.nexo_public.id
  route_table_id = aws_route_table.nexo_public.id
}

# --- security group ----------------------------------------------------
resource "aws_security_group" "nexo" {
  name   = "nexo"
  vpc_id = aws_vpc.nexo.id

  # SSH only from your home IP — replace 1.2.3.4/32 with yours.
  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["1.2.3.4/32"]
  }

  # 443 open to the world, terminated at nginx on the instance.
  ingress {
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  # 80 only to redirect to https.
  ingress {
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

# --- IAM role: SES + S3 backups, nothing else --------------------------
resource "aws_iam_role" "nexo" {
  name = "nexo-instance"
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action    = "sts:AssumeRole"
      Effect    = "Allow"
      Principal = { Service = "ec2.amazonaws.com" }
    }]
  })
}

resource "aws_iam_role_policy" "nexo" {
  name = "nexo-instance-policy"
  role = aws_iam_role.nexo.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      { Effect = "Allow", Action = ["ses:SendEmail","ses:SendRawEmail"], Resource = "*" },
      { Effect = "Allow", Action = ["s3:PutObject","s3:GetObject","s3:DeleteObject","s3:ListBucket"], Resource = ["arn:aws:s3:::your-nexo-backups","arn:aws:s3:::your-nexo-backups/*"] }
    ]
  })
}

resource "aws_iam_instance_profile" "nexo" {
  name = "nexo-instance"
  role = aws_iam_role.nexo.name
}

# --- AMI lookup: latest Debian 12 arm64 -------------------------------
data "aws_ami" "debian" {
  most_recent = true
  owners      = ["136693071363"]   # Debian official
  filter {
    name   = "name"
    values = ["debian-12-arm64-*"]
  }
}

# --- instance ----------------------------------------------------------
resource "aws_instance" "nexo" {
  ami                    = data.aws_ami.debian.id
  instance_type          = "t4g.small"
  subnet_id              = aws_subnet.nexo_public.id
  vpc_security_group_ids = [aws_security_group.nexo.id]
  iam_instance_profile   = aws_iam_instance_profile.nexo.name
  key_name               = "your-existing-aws-keypair-name"

  root_block_device {
    volume_size = 16
    volume_type = "gp3"
    encrypted   = true
  }

  tags = {
    Name = "nexo-1"
  }
}

# --- Route53 DNS -------------------------------------------------------
data "aws_route53_zone" "main" {
  name = "yourdomain.com."
}

resource "aws_route53_record" "nexo" {
  zone_id = data.aws_route53_zone.main.zone_id
  name    = "nexo.yourdomain.com"
  type    = "A"
  ttl     = 300
  records = [aws_instance.nexo.public_ip]
}

output "nexo_ip" {
  value = aws_instance.nexo.public_ip
}
```

Then:

```bash
terraform init
terraform apply
# review the plan; type 'yes'
```

## 2. Hardening + install (post-provision)

SSH in:

```bash
ssh admin@nexo.yourdomain.com
sudo apt update && sudo apt full-upgrade -y
sudo apt install -y unattended-upgrades ufw fail2ban nginx certbot python3-certbot-nginx
sudo dpkg-reconfigure -p low unattended-upgrades

# UFW — defense in depth on top of the security group
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable

# Disable root SSH + password auth
sudo sed -i 's/^#\?PermitRootLogin.*/PermitRootLogin no/' /etc/ssh/sshd_config
sudo sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config
sudo systemctl restart ssh
```

Install Nexo (when 27.4 .deb is available):

```bash
curl -LO https://github.com/lordmacu/nexo-rs/releases/latest/download/nexo-rs_arm64.deb
# Verify Cosign signature first (Phase 27.3) — see verify.md
sudo apt install ./nexo-rs_arm64.deb
```

NATS:

```bash
NATS_VERSION=2.10.20
curl -LO "https://github.com/nats-io/nats-server/releases/download/v${NATS_VERSION}/nats-server-v${NATS_VERSION}-linux-arm64.deb"
sudo apt install ./nats-server-v${NATS_VERSION}-linux-arm64.deb
sudo systemctl enable --now nats-server
```

## 3. nginx + ACM-via-certbot

```bash
sudo tee /etc/nginx/sites-available/nexo >/dev/null <<'EOF'
server {
    listen 80;
    server_name nexo.yourdomain.com;
    return 301 https://$server_name$request_uri;
}

server {
    listen 443 ssl http2;
    server_name nexo.yourdomain.com;

    # Cert paths populated after `certbot --nginx`
    ssl_certificate     /etc/letsencrypt/live/nexo.yourdomain.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/nexo.yourdomain.com/privkey.pem;
    ssl_protocols       TLSv1.2 TLSv1.3;

    # Health check — proxied through to the daemon
    location /health    { proxy_pass http://127.0.0.1:8080; access_log off; }
    location /ready     { proxy_pass http://127.0.0.1:8080; access_log off; }

    # Admin surface (auth via the daemon's session token)
    location /api/      { proxy_pass http://127.0.0.1:8080; }
    location /admin/    { proxy_pass http://127.0.0.1:8080; }

    # Block /metrics from public — scrape internally only
    location /metrics   { return 403; }
}
EOF
sudo ln -s /etc/nginx/sites-available/nexo /etc/nginx/sites-enabled/nexo
sudo nginx -t

# Issue cert (ACME via Let's Encrypt — same chain ACM uses)
sudo certbot --nginx -d nexo.yourdomain.com --non-interactive --agree-tos -m ops@yourdomain.com
sudo systemctl reload nginx
```

If you want **AWS ACM specifically** (instead of Let's Encrypt),
front the EC2 with an ALB and attach an ACM cert there — adds
~$18/mo for the ALB. Most personal deploys don't need it.

## 4. Wire SES for outbound email

The IAM role grants `ses:SendEmail`. Configure in `config/llm.yaml`:

```yaml
plugins:
  email:
    provider: ses
    aws_region: us-east-1
    # Credentials come from the EC2 instance profile — no keys
    # in the YAML.
    sender: "agent@nexo.yourdomain.com"
```

**Verify the sender domain in SES first:**

```bash
aws ses verify-domain-identity --domain yourdomain.com
# Add the printed TXT record to Route53
aws ses set-identity-mail-from-domain --identity yourdomain.com \
    --mail-from-domain mail.yourdomain.com
```

If your SES account is still in sandbox, request production
access via the SES console — required to send to non-verified
recipients.

## 5. EBS snapshots + lifecycle

```bash
# Daily snapshot via DLM (Data Lifecycle Manager) — set up once
# in Terraform or via the console:

aws dlm create-lifecycle-policy \
    --description "nexo daily snapshots, retain 30" \
    --state ENABLED \
    --execution-role-arn arn:aws:iam::ACCT:role/AWSDataLifecycleManagerDefaultRole \
    --policy-details '{...}'   # see DLM docs
```

Or the cheap way: cron + `aws ec2 create-snapshot` on the
instance itself, retaining 30 days locally.

## 6. CloudWatch logs + metrics

```bash
sudo apt install -y amazon-cloudwatch-agent
sudo /opt/aws/amazon-cloudwatch-agent/bin/amazon-cloudwatch-agent-config-wizard
# Point at /var/log/nexo-rs/*.log + 9090/metrics scrape
```

The Prometheus metrics endpoint can be pulled by CloudWatch
Container Insights via the EMF agent if you go in that
direction. For most personal deploys, journalctl + a Grafana
Cloud free-tier scrape is cheaper.

## Limits + escape hatches

- **t4g.small RAM (2 GB)** is tight if the browser plugin is on.
  Bump to `t4g.medium` (4 GB, ~$26/mo) before turning on Chrome.
- **Single AZ.** AZ outage = full downtime. Multi-AZ needs
  Phase 32 + an external NATS cluster. Acceptable for personal
  agents; not for SLAs.
- **SES sandbox limit (200 emails/day)** until you request
  production. Plan for this if email channel is primary.
- **EIP not allocated.** Stop/start the instance and the public
  IP changes. Allocate an Elastic IP (free when attached) if the
  Route53 record can't auto-update.

## Troubleshooting

- **Nexo can't send email** — `aws sts get-caller-identity` from
  the instance must show the `nexo-instance` role. If empty, the
  instance profile is missing.
- **certbot --nginx fails** — DNS hasn't propagated yet. Wait
  5-10 min after the Route53 record creation.
- **`/health` returns 503** — broker not ready. `systemctl
  status nats-server`; if good, check `journalctl -u nexo-rs`
  for credential errors (instance profile didn't propagate, or
  `config/llm.yaml` references a key the instance can't reach).

## Related

- [Hetzner Cloud](./deploy-hetzner.md) — bare-VM, cheaper
- [Fly.io](./deploy-fly.md) — easier scaling, less AWS lock-in
- Phase 27.4 (Debian package) — source of the .deb this recipe
  consumes
- Phase 27.3 (Cosign) — signature verification before install
