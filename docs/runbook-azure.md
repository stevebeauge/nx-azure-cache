# Deploying the Azure infrastructure

Everything is done by `scripts/deploy-azure.ps1`, which can be rerun, except the Azure DevOps
service connection. The values below are examples: replace them with yours.

## Prerequisites

- PowerShell 7.4 or later (`pwsh`) and Azure CLI.
- `az login --tenant <tenant>` with an account that can:
  - create resources and role assignments in the subscription;
  - create app registrations;
  - grant admin consent (Cloud Application Administrator or higher).
- An Entra group holding the developers (readers): its object id is passed as `-DevGroupId`.

## Run

```powershell
./scripts/deploy-azure.ps1 `
  -Subscription 00000000-0000-0000-0000-000000000000 `
  -Location westeurope `
  -Account mystorageaccount `
  -DevGroupId 00000000-0000-0000-0000-000000000000
```

| Parameter | Required | Default | Meaning |
|---|---|---|---|
| `-Subscription` | yes | | subscription id |
| `-Location` | yes | | Azure region |
| `-Account` | yes | | storage account name, globally unique |
| `-DevGroupId` | yes | | object id of the developers' Entra group |
| `-ResourceGroup` | no | `nx-azure-cache-rg` | resource group |
| `-Container` | no | `nx-cache` | blob container |
| `-DevAppName` | no | `nx-azure-cache` | developer app registration |
| `-CiAppName` | no | `nx-azure-cache-ci` | CI app registration |
| `-CiIssuer`, `-CiSubject` | no | | federated credential of the CI app, see below |

For a disposable storage account, pass other names:

```powershell
./scripts/deploy-azure.ps1 -Subscription <id> -Location westeurope -DevGroupId <id> `
  -ResourceGroup nx-azure-cache-test-rg -Account <other name>
```

The script creates what is missing:
- the storage account, without shared key, with last-access tracking and deletion after 90 days
  without access;
- the container (`nx-cache` by default);
- the developers' public app, with tenant-wide consent;
- the CI app and its principal;
- the roles:
  - developers as Storage Blob Data Reader on the container;
  - CI as Storage Blob Data Contributor on the container, and Reader on the resource group.

At the end it prints the developers' `client_id` and the CI client id. On a developer machine,
`config.toml` then needs `account`, `tenant_id` and `client_id` (the developers' one): the
Gateway compiles no default for them, and `login` fails, naming the missing keys, without them.

```toml
account = "mystorageaccount"
tenant_id = "00000000-0000-0000-0000-000000000000"
client_id = "00000000-0000-0000-0000-000000000000"
```

Entra accepts the redirect to `http://127.0.0.1:<port>` whatever the port, so the developer app
only registers `http://127.0.0.1`.

## Azure DevOps service connection (manual)

Done once, after a first run of the script.

1. *Project settings* > *Service connections* > *New* > *Azure Resource Manager*. Choose *App
   registration or Managed identity (manual)*, then *Workload identity federation*. Tenant: the
   Entra tenant. Scope: the subscription. *Application (client) ID*: the CI client id printed by
   the script. Do not tick *Grant access permission to all pipelines*.
2. Check that the **Issuer** starts with `https://login.microsoftonline.com/` (the
   `vstoken.dev.azure.com` issuer is retired on 2027-07-01). Copy the Issuer and the *Subject
   identifier*, then *Keep as draft*.
3. Rerun the script, with the same parameters, to add the federated credential:

   ```powershell
   ./scripts/deploy-azure.ps1 -Subscription <id> -Location westeurope -Account mystorageaccount `
     -DevGroupId <id> -CiIssuer "<Issuer>" -CiSubject "<Subject identifier>"
   ```

4. *Verify and save*. The verification reads the subscription: the Reader role on the resource
   group is enough. An `AuthorizationFailed` on `subscriptions/read` means it has not propagated
   yet: wait a few minutes.

## Tearing down a disposable storage account

```powershell
az group delete --name <disposable group> --yes
```

The developer and CI apps are shared with the long-lived deployment: do not delete them.
