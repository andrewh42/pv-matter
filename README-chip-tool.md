pv-matter odroid service is paired to chip-tool as node 0x60.

Endpoint map (see docs/architecture.md):

  1  Solar Power       composition parent
  2  Power Source      Status / WiredCurrentType
  3  Electrical Sensor Power Topology + EPM + EEM   <- the measurements
  4  Temperature       inverter internal temperature

To subscribe to everything on the power measurement endpoint (wildcard cluster
and attribute, min 1 s / max 60 s reporting interval, node 0x60, endpoint 3):

$ chip-tool interactive start
any subscribe-by-id 0xFFFFFFFF 0xFFFFFFFF 1 60 0x60 3

Note that reports only arrive when a value actually changes, so a quiet
endpoint at night is expected; instantaneous readings read as null once the
publisher reports asleep/offline, while energy/total is retained.
