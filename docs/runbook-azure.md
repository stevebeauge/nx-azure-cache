# Deploying the Azure infrastructure

Everything is done by `scripts/deploy-azure.ps1`, which can be rerun, except the CI side (Azure
DevOps service connection, GitHub Actions workflow). The values below are examples: replace them with yours.

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
| `-CiIssuer`, `-CiSubject` | no | | federated credentials of the CI app, see below |

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

## CI federated credentials

The CI authenticates as the CI app through workload identity federation, without a secret. Each
CI system (or repository, or service connection) is one federated credential on the CI app: an
**issuer** and a **subject**. `-CiIssuer` and `-CiSubject` take several values, paired by
position: the first issuer goes with the first subject, and so on.

The script adds each missing pair and leaves the others alone: a credential whose issuer and
subject match a pair is kept as is, whatever its name, and nothing is ever deleted. A rerun does
not need every pair again: passing one new pair adds that one only. The name of
a new credential is derived from the pair (the subject, then a hash of issuer and subject), so the
same inputs always give the same name. Entra accepts at most 20 federated credentials per app.

To remove a credential, delete it by hand:

```powershell
az ad app federated-credential list --id <CI client id> --query "[].{name:name, subject:subject}"
az ad app federated-credential delete --id <CI client id> --federated-credential-id <name>
```

Both CI systems below, in one run:

```powershell
./scripts/deploy-azure.ps1 -Subscription <id> -Location westeurope -Account mystorageaccount `
  -DevGroupId <id> `
  -CiIssuer "<Issuer of the service connection>", "https://token.actions.githubusercontent.com" `
  -CiSubject "<Subject identifier of the service connection>", "repo:my-org/my-repo:ref:refs/heads/main"
```

### Azure DevOps service connection (manual)

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

### GitHub Actions

GitHub needs nothing created in its settings: the subject is built from the repository and what
the job runs on.

| The job runs on | Subject |
|---|---|
| a branch | `repo:my-org/my-repo:ref:refs/heads/main` |
| a tag | `repo:my-org/my-repo:ref:refs/tags/v1.0.0` |
| pull requests | `repo:my-org/my-repo:pull_request` |
| an environment | `repo:my-org/my-repo:environment:production` |

The issuer is always `https://token.actions.githubusercontent.com`. A job that declares
`environment:` gets the environment subject, not the branch one. Each subject the CI uses is one
pair; for example, the `main` branch and pull requests:

```powershell
./scripts/deploy-azure.ps1 -Subscription <id> -Location westeurope -Account mystorageaccount `
  -DevGroupId <id> `
  -CiIssuer "https://token.actions.githubusercontent.com", "https://token.actions.githubusercontent.com" `
  -CiSubject "repo:my-org/my-repo:ref:refs/heads/main", "repo:my-org/my-repo:pull_request"
```

The workflow then needs `permissions: id-token: write` and the `AZURE_CLIENT_ID` (the CI client
id printed by the script), `AZURE_TENANT_ID` and `AZURE_SUBSCRIPTION_ID` it passes to
`azure/login`.

## Tearing down a disposable storage account

```powershell
az group delete --name <disposable group> --yes
```

The developer and CI apps are shared with the long-lived deployment: do not delete them.
