// Re-export for backward compatibility — many route files import these from client.
export { API_BASE, getToken, setToken, type PaginationMeta, type PaginatedResponse } from './_base.svelte';
export { connectJobLogWebSocket, connectNotificationWebSocket } from './websockets';
export { repos } from './repos';
export { packages } from './packages';
export { runners, type RegisterRunnerResponse } from './runners';
export { timeTracking } from './timeTracking';
export { boards } from './boards';
export { search, type SearchResponse, type SearchResult } from './search';
export { auth, type AuthLoginResponse, type PublicSsoProvider } from './auth';
export { attachments, type Attachment, type AttachmentTarget } from './attachments';
export { releases, type ReleaseAsset } from './releases';
export { issues } from './issues';
export { pulls, reviews } from './pulls';
export { pipelines } from './pipelines';
export { wiki } from './wiki';
export { collaborators } from './collaborators';
export { labels } from './labels';
export { notifications } from './notifications';
export { orgs } from './orgs';
export { branchProtections, type BranchProtectionPayload, type BranchProtectionRule } from './branchProtections';
export { mirrors, type MirrorPayload, type RepositoryMirror } from './mirrors';
export { webhooks, type RepositoryWebhook, type WebhookDelivery, type WebhookPayload } from './webhooks';
export { imports, type ImportTask, type StartImportPayload } from './imports';
export { milestones } from './milestones';
export { tokens } from './tokens';
export { sshKeys, type SshKey } from './sshKeys';
export { deployKeys, type DeployKey } from './deployKeys';
export { ciSecrets, type CiSecret } from './ciSecrets';
export { tagProtections, type TagProtection } from './tagProtections';
export { ciEnvironments, type CiEnvironment, type CiEnvironmentPayload } from './ciEnvironments';
export { ciRetention, type CiRetentionPolicy, type CiCleanupResult } from './ciRetention';
export { mfa, type MfaBackupStatus, type MfaEnableResponse, type MfaSetupResponse } from './mfa';
export {
  admin,
  type AdminOrg,
  type AdminSettings,
  type AdminSsoProvider,
  type AdminUser,
  type AuditLogEntry,
  type AuditLogQuery,
  type AuditLogResponse,
  type LoginAttemptEntry,
  type LoginAttemptQuery,
  type LoginAttemptResponse,
  type SsoProviderPayload,
  type UpdateUserData,
} from './admin';
