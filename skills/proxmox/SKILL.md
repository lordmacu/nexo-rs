---
name: Proxmox
description: Control de VMs/containers en Proxmox VE via REST API.
requires:
  bins: []
  env: [PROXMOX_URL, PROXMOX_TOKEN]
---

# Proxmox

Gestiona nodos/VMs/LXC en tu cluster Proxmox. Auth via API token
(`user@realm!tokenid=value`). Reads libres; lifecycle gated por
`PROXMOX_ALLOW_WRITE=true`.

## Tools
- `status`
- `list_nodes` — nodos + status
- `list_vms(node?)` — QEMU VMs
- `list_containers(node?)` — LXC
- `vm_status(node, vmid, kind?)` — status/current. kind = qemu|lxc (default qemu)
- `vm_action†(node, vmid, kind?, action)` — action ∈ start|stop|shutdown|reboot|suspend|resume

## Setup
Crear API token: Datacenter → Permissions → API Tokens.  
Setear `PROXMOX_URL=https://pve.local:8006`, `PROXMOX_TOKEN=root@pam!mytoken=abc-...`  
Si usás self-signed TLS: `PROXMOX_INSECURE_TLS=true`.
