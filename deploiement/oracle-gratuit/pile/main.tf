# Minelib sur les machines gratuites d'Oracle Cloud (v329) : pile pour Resource Manager.
#
# Crée en une fois le réseau, son pare-feu, un seau pour le code, et jusqu'à quatre
# machines de l'offre « Always Free ». Chacune s'installe toute seule (installer.sh) : la
# première construit Minelib depuis le code de la pile, puis toutes lancent leurs services.
# Voir LISEZMOI.md à côté de ce dossier.

terraform {
  required_version = ">= 1.2.0"
  required_providers {
    oci = {
      source = "oracle/oci"
    }
    tls = {
      source = "hashicorp/tls"
    }
  }
}

provider "oci" {
  region = var.region
}

# Remplis par Resource Manager.
variable "tenancy_ocid" {
  type = string
}

variable "compartment_ocid" {
  type = string
}

variable "region" {
  type = string
}

variable "nom" {
  type        = string
  default     = "Minelib"
  description = "Nom du relais officiel."
}

variable "duckdns_noms" {
  type        = string
  default     = ""
  description = "Deux noms DuckDNS (sans .duckdns.org), séparés par une virgule : un par machine Arm. Vide : pas de serveur relais d'iroh."
}

variable "duckdns_jeton" {
  type        = string
  default     = ""
  sensitive   = true
  description = "Jeton du compte DuckDNS (en haut de duckdns.org une fois connecté)."
}

variable "contact" {
  type        = string
  default     = ""
  description = "Adresse e-mail donnée à Let's Encrypt (facultative)."
}

variable "cle_ssh" {
  type        = string
  default     = ""
  description = "Clé SSH publique pour se connecter aux machines. Vide : une clé est créée (sortie cle_ssh_privee)."
}

variable "machines_arm" {
  type        = number
  default     = 2
  description = "Machines Arm (A1, 1 processeur et 6 Go chacune) : 1 ou 2."
}

variable "machines_amd" {
  type        = number
  default     = 2
  description = "Petites machines AMD (E2.1.Micro) : 0, 1 ou 2."
}

locals {
  ads = data.oci_identity_availability_domains.ads.availability_domains[*].name
  # (Les machines gratuites ne sont proposées que dans certains domaines de disponibilité.)
  ad_arm = try([for ad in local.ads : ad if contains(data.oci_core_shapes.formes[ad].shapes[*].name, "VM.Standard.A1.Flex")][0], local.ads[0])
  ad_amd = try([for ad in local.ads : ad if contains(data.oci_core_shapes.formes[ad].shapes[*].name, "VM.Standard.E2.1.Micro")][0], local.ads[0])
  noms   = [for n in split(",", lower(replace(var.duckdns_noms, " ", ""))) : trimsuffix(n, ".duckdns.org") if can(regex("^[a-z0-9-]+(\\.duckdns\\.org)?$", n))]
  # (Rien qui puisse sortir des guillemets des réglages.)
  nom     = replace(replace(replace(replace(var.nom, "'", ""), "\"", ""), "$", ""), "%", "")
  jeton   = replace(var.duckdns_jeton, "/[^A-Za-z0-9-]/", "")
  contact = replace(var.contact, "/[^A-Za-z0-9@._+-]/", "")
  image_arm = coalesce(
    try(data.oci_core_images.arm.images[0].id, ""),
    try(data.oci_core_images.arm_tout.images[0].id, ""),
  )
  image_amd = coalesce(
    try(data.oci_core_images.amd.images[0].id, ""),
    try(data.oci_core_images.amd_tout.images[0].id, ""),
  )
  cle_ssh = var.cle_ssh != "" ? var.cle_ssh : one(tls_private_key.ssh[*].public_key_openssh)
  # Le seau, vu des machines : lecture et écriture de ses objets, sans pouvoir les lister.
  seau = "https://objectstorage.${var.region}.oraclecloud.com${oci_objectstorage_preauthrequest.machines.access_uri}"
}

data "oci_identity_availability_domains" "ads" {
  compartment_id = var.tenancy_ocid
}

data "oci_core_shapes" "formes" {
  for_each            = toset(data.oci_identity_availability_domains.ads.availability_domains[*].name)
  compartment_id      = var.compartment_ocid
  availability_domain = each.value
}

data "oci_core_images" "arm" {
  compartment_id           = var.compartment_ocid
  operating_system         = "Canonical Ubuntu"
  operating_system_version = "24.04"
  shape                    = "VM.Standard.A1.Flex"
  sort_by                  = "TIMECREATED"
  sort_order               = "DESC"
  filter {
    name   = "display_name"
    values = ["^Canonical-Ubuntu-24\\.04-aarch64-[0-9.]+-[0-9]+$"]
    regex  = true
  }
}

data "oci_core_images" "arm_tout" {
  compartment_id           = var.compartment_ocid
  operating_system         = "Canonical Ubuntu"
  operating_system_version = "24.04"
  shape                    = "VM.Standard.A1.Flex"
  sort_by                  = "TIMECREATED"
  sort_order               = "DESC"
}

data "oci_core_images" "amd" {
  compartment_id           = var.compartment_ocid
  operating_system         = "Canonical Ubuntu"
  operating_system_version = "24.04"
  shape                    = "VM.Standard.E2.1.Micro"
  sort_by                  = "TIMECREATED"
  sort_order               = "DESC"
  filter {
    name   = "display_name"
    values = ["^Canonical-Ubuntu-24\\.04-[0-9.]+-[0-9]+$"]
    regex  = true
  }
}

data "oci_core_images" "amd_tout" {
  compartment_id           = var.compartment_ocid
  operating_system         = "Canonical Ubuntu"
  operating_system_version = "24.04"
  shape                    = "VM.Standard.E2.1.Micro"
  sort_by                  = "TIMECREATED"
  sort_order               = "DESC"
}

data "oci_objectstorage_namespace" "ns" {
  compartment_id = var.tenancy_ocid
}

resource "tls_private_key" "ssh" {
  count     = var.cle_ssh == "" ? 1 : 0
  algorithm = "ED25519"
}

# ───────────────────────────── Réseau ─────────────────────────────

resource "oci_core_vcn" "minelib" {
  compartment_id = var.compartment_ocid
  cidr_blocks    = ["10.42.0.0/16"]
  display_name   = "minelib"
  dns_label      = "minelib"
}

resource "oci_core_internet_gateway" "minelib" {
  compartment_id = var.compartment_ocid
  vcn_id         = oci_core_vcn.minelib.id
  display_name   = "minelib"
  enabled        = true
}

resource "oci_core_default_route_table" "minelib" {
  manage_default_resource_id = oci_core_vcn.minelib.default_route_table_id
  route_rules {
    destination       = "0.0.0.0/0"
    destination_type  = "CIDR_BLOCK"
    network_entity_id = oci_core_internet_gateway.minelib.id
  }
}

resource "oci_core_security_list" "minelib" {
  compartment_id = var.compartment_ocid
  vcn_id         = oci_core_vcn.minelib.id
  display_name   = "minelib"

  egress_security_rules {
    destination = "0.0.0.0/0"
    protocol    = "all"
  }

  # SSH, serveur relais d'iroh (HTTP, HTTPS), relais Minelib en TCP.
  dynamic "ingress_security_rules" {
    for_each = [22, 80, 443, 27650]
    content {
      source   = "0.0.0.0/0"
      protocol = "6"
      tcp_options {
        min = ingress_security_rules.value
        max = ingress_security_rules.value
      }
    }
  }

  # Serveur relais d'iroh : adresses vues de l'extérieur (QUIC).
  ingress_security_rules {
    source   = "0.0.0.0/0"
    protocol = "17"
    udp_options {
      min = 7842
      max = 7842
    }
  }

  # Passeurs : un port UDP par relais servi.
  ingress_security_rules {
    source   = "0.0.0.0/0"
    protocol = "17"
    udp_options {
      min = 40000
      max = 40019
    }
  }

  # Taille des paquets sur le chemin (sans quoi certaines liaisons se figent).
  ingress_security_rules {
    source   = "0.0.0.0/0"
    protocol = "1"
    icmp_options {
      type = 3
      code = 4
    }
  }

  # Entre les machines de la pile.
  ingress_security_rules {
    source   = "10.42.0.0/16"
    protocol = "all"
  }
}

resource "oci_core_subnet" "minelib" {
  compartment_id    = var.compartment_ocid
  vcn_id            = oci_core_vcn.minelib.id
  cidr_block        = "10.42.1.0/24"
  display_name      = "minelib"
  dns_label         = "machines"
  route_table_id    = oci_core_vcn.minelib.default_route_table_id
  security_list_ids = [oci_core_security_list.minelib.id]
}

# ───────────────────────────── Seau ─────────────────────────────
#
# Le code (source.tar.gz) et le script d'installation, puis ce que les machines y
# déposent : les programmes construits par la première, la ligne du relais officiel, et
# l'état de chacune (etat-*.txt).

resource "oci_objectstorage_bucket" "minelib" {
  compartment_id = var.compartment_ocid
  namespace      = data.oci_objectstorage_namespace.ns.namespace
  name           = "minelib-pile"
  access_type    = "NoPublicAccess"
}

resource "oci_objectstorage_object" "source" {
  namespace = data.oci_objectstorage_namespace.ns.namespace
  bucket    = oci_objectstorage_bucket.minelib.name
  object    = "source.tar.gz"
  source    = abspath("${path.module}/source.tar.gz")
  # (Une autre archive remplace l'objet : les machines la reconstruisent dans l'heure.)
  metadata = {
    "sha256" = filesha256("${path.module}/source.tar.gz")
  }
}

resource "oci_objectstorage_object" "installer" {
  namespace = data.oci_objectstorage_namespace.ns.namespace
  bucket    = oci_objectstorage_bucket.minelib.name
  object    = "installer.sh"
  content   = file("${path.module}/installer.sh")
}

# Les réglages du formulaire, relus par les machines à chaque mise à jour : les changer
# (Edit stack, Apply) suffit, sans refaire les machines.
resource "oci_objectstorage_object" "reglages" {
  namespace = data.oci_objectstorage_namespace.ns.namespace
  bucket    = oci_objectstorage_bucket.minelib.name
  object    = "reglages.env"
  content   = <<-FIN
    NOM='${local.nom}'
    DUCKDNS_NOMS='${join(",", local.noms)}'
    DUCKDNS_JETON='${local.jeton}'
    CONTACT='${local.contact}'
  FIN
}

resource "oci_objectstorage_preauthrequest" "machines" {
  namespace             = data.oci_objectstorage_namespace.ns.namespace
  bucket                = oci_objectstorage_bucket.minelib.name
  name                  = "machines-minelib"
  access_type           = "AnyObjectReadWrite"
  bucket_listing_action = "Deny"
  time_expires          = "2036-01-01T00:00:00Z"
}

# ───────────────────────────── Machines ─────────────────────────────

# Arm : la première (« relais ») construit tout et porte le relais officiel ; la seconde
# (« relais2 ») un second serveur relais d'iroh. Toutes deux sont aussi des passeurs.
resource "oci_core_instance" "arm" {
  count               = var.machines_arm
  availability_domain = local.ad_arm
  compartment_id      = var.compartment_ocid
  display_name        = count.index == 0 ? "minelib-relais" : "minelib-relais2"
  shape               = "VM.Standard.A1.Flex"

  shape_config {
    ocpus         = 1
    memory_in_gbs = 6
  }

  source_details {
    source_type = "image"
    source_id   = local.image_arm
    # (200 Go gratuits en tout : 50 par machine, quatre machines.)
    boot_volume_size_in_gbs = 50
  }

  create_vnic_details {
    subnet_id        = oci_core_subnet.minelib.id
    assign_public_ip = true
    hostname_label   = count.index == 0 ? "relais" : "relais2"
  }

  metadata = {
    ssh_authorized_keys = local.cle_ssh
    user_data = base64encode(templatefile("${path.module}/amorce.sh", {
      role = count.index == 0 ? "relais" : "relais2"
      seau = local.seau
    }))
  }

  # Une nouvelle image d'Ubuntu, ou une pile mise à jour, ne refait pas les machines : elles
  # gardent leur identité (noeud.key) et leurs comptes. Les mises à jour du code passent
  # par le seau.
  lifecycle {
    ignore_changes = [metadata, source_details, availability_domain, defined_tags]
  }

  depends_on = [
    oci_objectstorage_object.source,
    oci_objectstorage_object.installer,
    oci_objectstorage_object.reglages,
  ]
}

# AMD : passeurs seulement (elles n'ont pas assez de mémoire pour construire le code : elles
# prennent les programmes que la première machine Arm a construits pour elles).
resource "oci_core_instance" "amd" {
  count               = var.machines_amd
  availability_domain = local.ad_amd
  compartment_id      = var.compartment_ocid
  display_name        = "minelib-passeur${count.index + 1}"
  shape               = "VM.Standard.E2.1.Micro"

  source_details {
    source_type             = "image"
    source_id               = local.image_amd
    boot_volume_size_in_gbs = 50
  }

  create_vnic_details {
    subnet_id        = oci_core_subnet.minelib.id
    assign_public_ip = true
    hostname_label   = "passeur${count.index + 1}"
  }

  metadata = {
    ssh_authorized_keys = local.cle_ssh
    user_data = base64encode(templatefile("${path.module}/amorce.sh", {
      role = "passeur"
      seau = local.seau
    }))
  }

  lifecycle {
    ignore_changes = [metadata, source_details, availability_domain, defined_tags]
  }

  depends_on = [
    oci_objectstorage_object.source,
    oci_objectstorage_object.installer,
    oci_objectstorage_object.reglages,
  ]
}

# ───────────────────────────── Sorties ─────────────────────────────

output "adresses_ip" {
  value = concat(oci_core_instance.arm[*].public_ip, oci_core_instance.amd[*].public_ip)
}

output "ligne_relais_officiel" {
  description = "Ligne pour relais_officiels.txt (le fichier existe une fois le relais lancé, environ 45 minutes après)."
  value       = "${local.seau}ligne.txt"
}

output "etat_relais" {
  description = "Où en est la première machine (construction, services)."
  value       = "${local.seau}etat-relais.txt"
}

output "relais_iroh" {
  description = "Lignes pour relais_iroh.txt."
  value       = join(" ", [for n in local.noms : "https://${n}.duckdns.org"])
}

output "cle_ssh_privee" {
  value     = one(tls_private_key.ssh[*].private_key_openssh)
  sensitive = true
}
