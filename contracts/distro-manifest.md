# Nopal distro manifest seed

Status: design seed; not yet an implemented manifest format.
Only execution and memory remain inter-product contracts, while the config/envelope and process/proof surfaces are Nopal's own.

Nopal is a distribution for Pi.
A bundle manifest should pin three things at bootstrap time:

1. **Extension packages**: Pi-visible UI/routing packages and skills.
2. **Core implementations**: `nopal` binary, Rondo Core service, beislid tools, Memento provider.
3. **Surface/contract claims**: which product-surface or contract versions each package produces or consumes.

## Draft shape

```jsonc
{
  "kind": "nopal.distro_manifest/v0",
  "bundle": {
    "name": "nopal-local",
    "version": "0.1.0"
  },
  "surfaces": {
    "config_and_envelopes": { "requires": ["nopal.gates/v1", "nopal.policy/v1", "nopal.workflow/v1", "nopal.integrations/v1", "nopal.guidance/v1"], "provider": "nopal-core" },
    "process_and_proof": { "requires": ["beislid-process-artifact-v1", "execution-envelope-v0"], "provider": "beislid" }
  },
  "contracts": {
    "execution": { "requires": ["rondo.core/v1"], "provider": "rondo-core", "status": "provisional" },
    "memory": { "requires": ["nopal.memory_provider/v1"], "provider": "memento", "status": "provisional" }
  },
  "packages": {
    "nopal-core": { "version": "0.1.0", "path": "./target/release/nopal" },
    "rondo-core": { "version": "pending", "contract_status": "provisional" },
    "beislid": { "version": "0.4.x" },
    "memento": { "version": "pending", "contract_status": "provisional" }
  }
}
```

## Bootstrap rule

Bootstrap validates the manifest before enabling the field extension:

- every package exists and reports a compatible version
- every required surface or contract version has a catalog/docs entry
- every non-provisional contract has a conformance runner result or a trusted release attestation
- provisional contracts can be installed for dogfood only and must be shown as provisional in the operator surface

The distro manifest never becomes a runtime authority for gate selection, policy decisions, proof verdicts, or memory ranking.
Those decisions stay in their owning surface or contract.
