# nx-azure-cache

Remote cache for Nx monorepos, stored in Azure Blob Storage and served to Nx by a process local to each machine.

## Language

**Gateway**:
Local process, one instance per machine, that speaks the Nx remote cache protocol on its input side and reads/writes the Blob on its output side, on behalf of an **Identity**.
_Avoid_: cache server, proxy

**Workspace**:
Nx monorepo consuming the cache, designated by a short identifier that prefixes its **Entries**.
_Avoid_: repo, project (a project is an Nx project inside a workspace)

**Entry**:
Result of an Nx task stored in the cache, addressed by a hash, immutable, treated as opaque bytes.
_Avoid_: artifact, blob (the blob is the Azure object that stores it)

**Hit** / **Miss**:
A requested **Entry** is served / is not served. Any read failure is seen by Nx as a **Miss**.

**Safe fallback**:
Guarantee that no remote cache failure (gateway missing, identity missing, Blob unreachable) fails an Nx run: it falls back to its local cache.

**Identity**:
Entra principal on whose behalf the **Gateway** accesses the Blob. It is a **reader** (developer machines) or a **writer** (CI).
_Avoid_: account, user

**Local token**:
Random value specific to one user of a machine, shared between the **Activation plugin** and the **Gateway**. It prevents other local users from using the gateway. It is not an Entra token.

**Activation plugin**:
Nx plugin that, before tasks run, sets the remote cache URL if the **Gateway** is healthy, and sets nothing otherwise.

## Relationships

- A machine runs at most one **Gateway**, which serves several **Workspaces** and several worktrees.
- An **Entry** belongs to exactly one **Workspace**. Two writes of the same hash: the first one wins.
- Only a writer **Identity** creates **Entries**.

## Flagged ambiguities

- "Container": there is a single Azure container for all **Workspaces**. A workspace is not a container.
