import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  fetchConfig,
  fetchDashboard,
  fetchStatus,
  fetchWorkspacePassword,
  regenerateWorkspacePassword,
  restartAgent,
  saveWorkspaceConfig
} from '../api';
import type { ConfigUpdatePayload } from '../types';

const keys = {
  status: ['management-status'] as const,
  dashboard: (workspaceId: string) => ['management-dashboard', workspaceId || 'primary'] as const,
  config: ['management-config'] as const,
  password: (workspaceId: string) => ['workspace-password', workspaceId] as const
};

export function useAgentQueries(selectedWorkspaceId: string) {
  const queryClient = useQueryClient();
  const status = useQuery({
    queryKey: keys.status,
    queryFn: ({ signal }) => fetchStatus(signal),
    refetchInterval: 5_000
  });
  const dashboard = useQuery({
    queryKey: keys.dashboard(selectedWorkspaceId),
    queryFn: ({ signal }) => fetchDashboard(selectedWorkspaceId || undefined, signal),
    refetchInterval: 5_000
  });
  const config = useQuery({
    queryKey: keys.config,
    queryFn: ({ signal }) => fetchConfig(signal),
    staleTime: Number.POSITIVE_INFINITY
  });
  const password = useQuery({
    queryKey: keys.password(selectedWorkspaceId),
    queryFn: ({ signal }) => fetchWorkspacePassword(selectedWorkspaceId, signal),
    enabled: Boolean(selectedWorkspaceId),
    staleTime: Number.POSITIVE_INFINITY
  });
  const save = useMutation({
    mutationFn: ({ workspaceId, payload }: { workspaceId: string; payload: ConfigUpdatePayload }) => (
      saveWorkspaceConfig(workspaceId, payload)
    ),
    onSuccess: async (_result, variables) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: keys.config }),
        queryClient.invalidateQueries({ queryKey: keys.status }),
        queryClient.invalidateQueries({ queryKey: keys.dashboard(variables.workspaceId) })
      ]);
    }
  });
  const regeneratePassword = useMutation({
    mutationFn: (workspaceId: string) => regenerateWorkspacePassword(workspaceId),
    onSuccess: async (_result, workspaceId) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: keys.password(workspaceId) }),
        queryClient.invalidateQueries({ queryKey: keys.config })
      ]);
    }
  });
  const restart = useMutation({ mutationFn: restartAgent });

  const refresh = async () => {
    await Promise.all([
      status.refetch(),
      dashboard.refetch(),
      config.refetch(),
      ...(selectedWorkspaceId ? [password.refetch()] : [])
    ]);
  };

  return {
    status,
    dashboard,
    config,
    password,
    save,
    regeneratePassword,
    restart,
    refresh,
    isRefreshing: status.isFetching || dashboard.isFetching || config.isFetching || password.isFetching
  };
}
