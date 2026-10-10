import { vi } from 'vitest';

export { ApiError } from '../api/error';

export {
	buildAdminUserPayload,
	type AdminUserFormState,
} from '../api/adminUserForm';
export {
	buildBoardCardUpdatePayload,
	buildBoardUpdatePayload,
	buildColumnUpdatePayload,
} from '../api/boardForm';
export {
	buildBranchProtectionPayload,
	parseStoredStringList,
	parseStringList,
} from '../api/branchProtectionForm';
export { buildIssueLinksPayload } from '../api/issueForm';
export { splitList } from '../api/tokens';
export { buildLabelPayload } from '../api/labelForm';
export {
	buildMilestoneCreatePayload,
	buildMilestoneUpdatePayload,
	dueDateForInput,
} from '../api/milestoneForm';
export { buildMirrorPayload } from '../api/mirrorForm';
export { buildRepoSettingsPatch, repoSettingsFormState } from '../api/repoSettingsForm';
export { EMAIL_NOTIFICATION_KEYS } from '../api/notifications';
export { buildOrganizationUpdatePayload } from '../api/orgManagement';
export { buildReleaseUpdatePayload } from '../api/releaseForm';
export { buildTagProtectionPayload } from '../api/tagProtectionForm';
export { allowedUserLabel, buildUserRef } from '../api/userRef';

type MockNamespace = Record<string, any>;

const namespaces: MockNamespace[] = [];

function namespace(): MockNamespace {
	const target: MockNamespace = {};
	const proxy = new Proxy(target, {
		get(current, property) {
			if (property === 'then') return undefined;
			if (typeof property !== 'string') return Reflect.get(current, property);
			current[property] ??= vi.fn();
			return current[property];
		},
	});
	namespaces.push(proxy);
	return proxy;
}

export const admin = namespace();
export const artifacts = namespace();
export const attachments = namespace();
export const auth = namespace();
export const boards = namespace();
export const bots = namespace();
export const branchProtections = namespace();
export const ciEnvironments = namespace();
export const ciRetention = namespace();
export const ciSecrets = namespace();
export const collaborators = namespace();
export const deployKeys = namespace();
export const lfsLocks = namespace();
export const lfsStorage = namespace();
export const imports = namespace();
export const instance = namespace();
export const issues = namespace();
export const labels = namespace();
export const mfa = namespace();
export const milestones = namespace();
export const mirrors = namespace();
export const notifications = namespace();
export const orgs = namespace();
export const packages = namespace();
export const passkeys = namespace();
export const pipelines = namespace();
export const pulls = namespace();
export const releases = namespace();
releases.attestation = namespace();
export const repos = namespace();
repos.templates = namespace();
export const reviews = namespace();
export const runners = namespace();
export const search = namespace();
export const signingKeys = namespace();
export const sshKeys = namespace();
export const tagProtections = namespace();
export const timeTracking = namespace();
export const tokens = namespace();
export const webhooks = namespace();
export const wiki = namespace();

export const connectJobLogWebSocket = vi.fn();
export const connectNotificationWebSocket = vi.fn();
export const getToken = vi.fn(() => 'test-token');
export const isPasskeySupported = vi.fn(() => true);
export const setToken = vi.fn();
export const API_BASE = '/api/v1';

export function resetTestClient(): void {
	for (const client of namespaces) {
		for (const fn of Object.values(client)) {
			if (typeof fn?.mockReset === 'function') fn.mockReset();
		}
	}
	for (const fn of [
		connectJobLogWebSocket,
		connectNotificationWebSocket,
		getToken,
		isPasskeySupported,
		setToken,
	]) {
		fn.mockReset();
	}
	getToken.mockReturnValue('test-token');
	isPasskeySupported.mockReturnValue(true);
}
