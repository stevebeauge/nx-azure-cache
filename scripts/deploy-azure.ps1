#requires -Version 7.4
<#
Deploys the remote cache's Azure infrastructure. Idempotent: each step creates what is missing
and leaves the rest in place.

Prerequisite: `az login --tenant <tenant>` with an account that can create resources and role
assignments in the subscription, create app registrations and grant admin consent
(Cloud Application Administrator or higher).

The only step outside the script: the Azure DevOps service connection (see docs/runbook-azure.md).
Once it exists, rerun the script with -CiIssuer and -CiSubject to add the federated credential.

Example:
  ./scripts/deploy-azure.ps1 -Subscription 00000000-0000-0000-0000-000000000000 `
    -Location westeurope -Account mystorageaccount -DevGroupId 00000000-0000-0000-0000-000000000000
#>
param(
  [Parameter(Mandatory)][string]$Subscription,  # subscription id
  [Parameter(Mandatory)][string]$Location,      # Azure region, e.g. westeurope
  [Parameter(Mandatory)][string]$Account,       # storage account name, globally unique
  [Parameter(Mandatory)][string]$DevGroupId,    # object id of the Entra group of developers (readers)
  [string]$ResourceGroup = "nx-azure-cache-rg",
  [string]$Container = "nx-cache",
  [string]$DevAppName = "nx-azure-cache",
  [string]$CiAppName = "nx-azure-cache-ci",
  [string]$CiIssuer,
  [string]$CiSubject
)
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true  # a failing az stops the script

$StorageApi = "e406a681-f3d4-42a8-90b6-c2b029497af1"  # Azure Storage
$GraphApi = "00000003-0000-0000-c000-000000000000"    # Microsoft Graph

# Exact filter: `az ad app list --display-name` filters by prefix.
function Get-AppId($name) { az ad app list --filter "displayName eq '$name'" --query "[0].appId" -o tsv }
function Get-ServicePrincipalId($appId) {
  $id = az ad sp list --filter "appId eq '$appId'" --query "[0].id" -o tsv
  if (-not $id) { $id = az ad sp create --id $appId --query id -o tsv }
  $id
}
function Get-ScopeId($api, $value) {
  az ad sp show --id $api --query "oauth2PermissionScopes[?value=='$value'].id" -o tsv
}
function Write-JsonFile($object) {
  $file = New-TemporaryFile
  ConvertTo-Json -InputObject $object -Depth 10 | Set-Content $file
  $file
}

az account set --subscription $Subscription

"Storage"
az group create --name $ResourceGroup --location $Location --output none
# No shared key: only Entra grants access to the data.
az storage account create --name $Account --resource-group $ResourceGroup --location $Location `
  --sku Standard_LRS --allow-shared-key-access false --output none
# Last-access tracking before any write, then deletion after 90 days without access.
az storage account blob-service-properties update --resource-group $ResourceGroup `
  --account-name $Account --enable-last-access-tracking true --output none
$policy = Write-JsonFile @{
  rules = @(@{
      enabled = $true; name = "nx-cache-delete-90d-without-access"; type = "Lifecycle"
      definition = @{
        filters = @{ blobTypes = @("blockBlob"); prefixMatch = @("$Container/") }
        actions = @{ baseBlob = @{ delete = @{ daysAfterLastAccessTimeGreaterThan = 90 } } }
      }
    })
}
az storage account management-policy create --resource-group $ResourceGroup `
  --account-name $Account --policy "@$policy" --output none
Remove-Item $policy
az storage container-rm create --resource-group $ResourceGroup --storage-account $Account `
  --name $Container --output none

"Developer app"
# The Gateway requests `https://storage.azure.com/.default openid profile offline_access`.
$permissions = Write-JsonFile @(
  @{ resourceAppId = $StorageApi; resourceAccess = @(@{ id = (Get-ScopeId $StorageApi "user_impersonation"); type = "Scope" }) }
  @{ resourceAppId = $GraphApi; resourceAccess = @("openid", "profile", "offline_access" | ForEach-Object { @{ id = (Get-ScopeId $GraphApi $_); type = "Scope" } }) }
)
az ad app create --display-name $DevAppName --sign-in-audience AzureADMyOrg `
  --is-fallback-public-client true --public-client-redirect-uris "http://127.0.0.1" `
  --required-resource-accesses "@$permissions" --output none
Remove-Item $permissions
$devAppId = Get-AppId $DevAppName
$null = Get-ServicePrincipalId $devAppId
# Tenant-wide consent: no developer has to consent on first login.
az ad app permission grant --id $devAppId --api $StorageApi --scope user_impersonation --output none
az ad app permission grant --id $devAppId --api $GraphApi --scope openid profile offline_access --output none

"CI identity"
az ad app create --display-name $CiAppName --sign-in-audience AzureADMyOrg --output none
$ciAppId = Get-AppId $CiAppName
$ciPrincipalId = Get-ServicePrincipalId $ciAppId
if ($CiIssuer -and $CiSubject) {
  $existing = az ad app federated-credential list --id $ciAppId `
    --query "[?subject=='$CiSubject'].id" -o tsv
  if (-not $existing) {
    $credential = Write-JsonFile @{
      name = "azure-devops"; issuer = $CiIssuer; subject = $CiSubject
      audiences = @("api://AzureADTokenExchange")
    }
    az ad app federated-credential create --id $ciAppId --parameters "@$credential" --output none
    Remove-Item $credential
  }
}

"Roles"
$accountId = az storage account show --resource-group $ResourceGroup --name $Account --query id -o tsv
$containerScope = "$accountId/blobServices/default/containers/$Container"
$rgId = az group show --name $ResourceGroup --query id -o tsv
function Grant($role, $principal, $type, $scope) {
  az role assignment create --role $role --assignee-object-id $principal `
    --assignee-principal-type $type --scope $scope --output none
}
Grant "Storage Blob Data Reader" $DevGroupId Group $containerScope
Grant "Storage Blob Data Contributor" $ciPrincipalId ServicePrincipal $containerScope
# Azure DevOps (Verify and save) and AzureCLI@2 read the connection's subscription.
Grant "Reader" $ciPrincipalId ServicePrincipal $rgId

""
"Developer client_id (config.toml): $devAppId"
"CI client id (Azure DevOps): $ciAppId"
