// Re-export for backward compatibility — many route files import these from client.
export { API_BASE, getToken, setToken, type PaginationMeta, type PaginatedResponse } from './_base.svelte';
export { ApiError } from './error';
export { connectJobLogWebSocket, connectNotificationWebSocket } from './websockets';
export {
  repos,
  type CommitSignature,
  type RepoPermission,
  type RepoSettingsPatch,
  type RepositoryDetail,
  type RepositoryFork,
  type RepoTreeEntry,
  type RepoTreeEntryKind,
  type SignatureVerdict,
  type Stargazer,
} from './repos';
export { packages } from './packages';
export { runners, type RegisterRunnerResponse } from './runners';
export { timeTracking } from './timeTracking';
export {
  boards,
  type Board,
  type BoardCard,
  type BoardColumn,
  type BoardFullResponse,
} from './boards';
export {
  buildBoardCardUpdatePayload,
  buildBoardUpdatePayload,
  buildColumnUpdatePayload,
  type BoardCardEditFormState,
  type BoardEditFormState,
} from './boardForm';
export { search, type SearchResponse, type SearchResult } from './search';
export { auth, type AuthLoginResponse, type Me, type PublicSsoProvider, type SsoLink } from './auth';
export { attachments, type Attachment, type AttachmentTarget } from './attachments';
export {
  releases,
  type AttestationEnvelope,
  type AttestationReport,
  type AttestationStatus,
  type ReleaseAsset,
} from './releases';
export {
  buildReleaseUpdatePayload,
  type ReleaseUpdateFormState,
  type ReleaseUpdatePayload
} from './releaseForm';
export { issues, type Issue, type IssueComment, type IssueUpdatePayload } from './issues';
export { buildIssueLinksPayload, type IssueLinksFormState } from './issueForm';
export { pulls, reviews } from './pulls';
export { pipelines } from './pipelines';
export { artifacts, type CiArtifact } from './artifacts';
export { wiki } from './wiki';
export { collaborators, type Collaborator } from './collaborators';
export { labels, type LabelPayload } from './labels';
export { buildLabelPayload, type LabelFormState } from './labelForm';
export {
  notifications,
  EMAIL_NOTIFICATION_KEYS,
  type EmailNotificationKey,
  type EmailNotificationSettings,
  type NotificationReason,
  type NotificationRow,
  type NotificationSettings,
  type SubscriptionReason,
  type ThreadKind,
  type ThreadSubscription,
} from './notifications';
export {
  orgs,
  type Organization,
  type OrganizationMember,
  type OrganizationMemberRole,
  type OrganizationTeam,
  type OrganizationUpdatePayload,
  type OrganizationVisibility,
  type TeamMember,
  type TeamMemberRole,
  type TeamPermission,
} from './orgs';
export {
  buildOrganizationUpdatePayload,
  type OrganizationEditFormState,
} from './orgManagement';
export { allowedUserLabel, buildUserRef, type AllowedUser, type UserRefPayload } from './userRef';
export { branchProtections, type BranchProtectionPayload, type BranchProtectionRule } from './branchProtections';
export {
  buildBranchProtectionPayload,
  parseStoredStringList,
  parseStringList,
  type BranchProtectionFormState,
  type StoredStringListParseResult
} from './branchProtectionForm';
export { mirrors, type MirrorPayload, type RepositoryMirror } from './mirrors';
export { buildMirrorPayload, type MirrorFormState } from './mirrorForm';
export {
  buildRepoSettingsPatch,
  repoSettingsFormState,
  type RepoSettingsFormState,
} from './repoSettingsForm';
export { webhooks, type RepositoryWebhook, type WebhookDelivery, type WebhookPayload } from './webhooks';
export { imports, type ImportTask, type StartImportPayload } from './imports';
export {
  milestones,
  type CreateMilestonePayload,
  type Milestone,
  type UpdateMilestonePayload,
} from './milestones';
export {
  buildMilestoneCreatePayload,
  buildMilestoneUpdatePayload,
  dueDateForInput,
  type MilestoneFormState,
} from './milestoneForm';
export { tokens, splitList, type TokenNarrowing, type TokenNarrowingFields } from './tokens';
export { bots, type Bot, type BotToken } from './bots';
export { sshKeys, type SshKey } from './sshKeys';
export { deployKeys, type DeployKey } from './deployKeys';
export { lfsLocks, type LfsLock } from './lfsLocks';
export { lfsStorage, type LfsObject, type LfsPruneOutcome, type LfsUsage } from './lfsStorage';
export {
  repoStorage,
  type RepoStorageLimits,
  type RepoStorageReport,
  type RepoStorageUsage,
} from './repoStorage';
export { ciSecrets, type CiSecret } from './ciSecrets';
export { tagProtections, type TagProtection, type TagProtectionPayload } from './tagProtections';
export { buildTagProtectionPayload, type TagProtectionFormState } from './tagProtectionForm';
export { ciEnvironments, type CiEnvironment, type CiEnvironmentPayload } from './ciEnvironments';
export { ciRetention, type CiRetentionPolicy, type CiCleanupResult } from './ciRetention';
export { instance, type InstanceInfo } from './instance';
export {
  mfa,
  type MfaBackupStatus,
  type MfaEnableResponse,
  type MfaRegenerateBackupResponse,
  type MfaSetupResponse,
} from './mfa';
export { passkeys, isPasskeySupported, type PasskeyInfo, type PasskeyLoginResponse } from './passkeys';
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
export { buildAdminUserPayload, type AdminUserFormState } from './adminUserForm';
