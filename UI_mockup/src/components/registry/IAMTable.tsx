import { useState } from 'react';
import { Copy, Check } from 'lucide-react';

interface User {
  id: string;
  name: string;
  email: string;
  type: 'user' | 'service' | 'token';
  created: string;
}

interface Resource {
  id: string;
  name: string;
  type: string;
}

interface Permission {
  userId: string;
  resourceId: string;
  read: boolean;
  write: boolean;
  execute: boolean;
}

export function IAMTable() {
  const [view, setView] = useState<'users' | 'matrix'>('matrix');
  const [copiedCell, setCopiedCell] = useState<string | null>(null);

  const users: User[] = [
    { id: 'u1', name: 'Alice', email: 'alice@orbit.sys', type: 'user', created: '2024-01-15' },
    { id: 'u2', name: 'Bob', email: 'bob@orbit.sys', type: 'user', created: '2024-02-20' },
    { id: 'u3', name: 'CI/CD Bot', email: 'cicd@orbit.sys', type: 'service', created: '2024-01-10' },
    { id: 'u4', name: 'Analytics Token', email: 'analytics-key-7a9f', type: 'token', created: '2024-03-01' }
  ];

  const resources: Resource[] = [
    { id: 'r1', name: 'S3 Bucket A', type: 'storage' },
    { id: 'r2', name: 'NVMe Namespace B', type: 'storage' },
    { id: 'r3', name: 'Capsule-Prod', type: 'compute' },
    { id: 'r4', name: 'Encryption Keys', type: 'security' }
  ];

  const [permissions, setPermissions] = useState<Permission[]>([
    { userId: 'u1', resourceId: 'r1', read: true, write: true, execute: false },
    { userId: 'u1', resourceId: 'r2', read: true, write: false, execute: false },
    { userId: 'u1', resourceId: 'r3', read: true, write: true, execute: true },
    { userId: 'u2', resourceId: 'r1', read: true, write: false, execute: false },
    { userId: 'u2', resourceId: 'r4', read: true, write: false, execute: false },
    { userId: 'u3', resourceId: 'r1', read: true, write: true, execute: true },
    { userId: 'u3', resourceId: 'r2', read: true, write: true, execute: true },
    { userId: 'u3', resourceId: 'r3', read: true, write: true, execute: true },
    { userId: 'u4', resourceId: 'r1', read: true, write: false, execute: false }
  ]);

  const getPermission = (userId: string, resourceId: string) => {
    return permissions.find(p => p.userId === userId && p.resourceId === resourceId) || 
      { userId, resourceId, read: false, write: false, execute: false };
  };

  const togglePermission = (userId: string, resourceId: string, type: 'read' | 'write' | 'execute') => {
    setPermissions(prev => {
      const existing = prev.find(p => p.userId === userId && p.resourceId === resourceId);
      if (existing) {
        return prev.map(p => 
          p.userId === userId && p.resourceId === resourceId
            ? { ...p, [type]: !p[type] }
            : p
        );
      } else {
        return [...prev, { userId, resourceId, read: type === 'read', write: type === 'write', execute: type === 'execute' }];
      }
    });
  };

  const copyToClipboard = (text: string, id: string) => {
    navigator.clipboard.writeText(text);
    setCopiedCell(id);
    setTimeout(() => setCopiedCell(null), 2000);
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="bg-white border-b border-gray-200 px-6 py-4 flex items-center justify-between flex-shrink-0">
        <div>
          <h2 className="text-gray-900 tracking-tight">Identity & Access Management</h2>
          <p className="text-gray-500 text-sm mt-1">Manage users, service accounts, and permissions</p>
        </div>
        
        <div className="flex gap-2">
          <button
            onClick={() => setView('users')}
            className={`px-4 py-2 text-sm border transition-colors ${
              view === 'users' 
                ? 'bg-gray-900 text-white border-gray-900' 
                : 'bg-white text-gray-700 border-gray-300 hover:bg-gray-50'
            }`}
          >
            Users List
          </button>
          <button
            onClick={() => setView('matrix')}
            className={`px-4 py-2 text-sm border transition-colors ${
              view === 'matrix' 
                ? 'bg-gray-900 text-white border-gray-900' 
                : 'bg-white text-gray-700 border-gray-300 hover:bg-gray-50'
            }`}
          >
            Permission Matrix
          </button>
          <button className="px-4 py-2 text-sm border border-gray-300 bg-white text-gray-700 hover:bg-gray-50">
            Export CSV
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto bg-gray-50">
        {view === 'users' ? (
          <div className="p-6">
            <table className="w-full bg-white border border-gray-200">
              <thead>
                <tr className="bg-gray-50 border-b border-gray-200">
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Name</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Email</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Type</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Created</th>
                  <th className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase">Actions</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-200">
                {users.map(user => (
                  <tr key={user.id} className="hover:bg-gray-50 h-[30px]">
                    <td className="px-4 py-2 text-sm text-gray-900">{user.name}</td>
                    <td className="px-4 py-2 text-sm text-gray-600 font-mono flex items-center gap-2">
                      <span>{user.email}</span>
                      <button
                        onClick={() => copyToClipboard(user.email, `email-${user.id}`)}
                        className="text-gray-400 hover:text-gray-600"
                      >
                        {copiedCell === `email-${user.id}` ? (
                          <Check className="w-3 h-3 text-green-600" />
                        ) : (
                          <Copy className="w-3 h-3" />
                        )}
                      </button>
                    </td>
                    <td className="px-4 py-2 text-sm">
                      <span className={`inline-flex px-2 py-1 text-xs ${
                        user.type === 'user' ? 'bg-blue-100 text-blue-800' :
                        user.type === 'service' ? 'bg-purple-100 text-purple-800' :
                        'bg-gray-100 text-gray-800'
                      }`}>
                        {user.type.toUpperCase()}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-sm text-gray-600 font-mono">{user.created}</td>
                    <td className="px-4 py-2 text-sm">
                      <button className="text-gray-600 hover:text-gray-900 text-xs">Edit</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="p-6">
            <div className="bg-white border border-gray-200 overflow-auto">
              <table className="w-full">
                <thead>
                  <tr className="bg-gray-50 border-b border-gray-200">
                    <th className="sticky left-0 z-10 bg-gray-50 px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase border-r border-gray-300">
                      User / Resource
                    </th>
                    {resources.map(resource => (
                      <th key={resource.id} className="px-4 py-3 text-left text-xs text-gray-700 tracking-wider uppercase border-r border-gray-200">
                        <div>{resource.name}</div>
                        <div className="text-gray-500 normal-case">{resource.type}</div>
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {users.map(user => (
                    <tr key={user.id} className="border-b border-gray-200 hover:bg-gray-50 h-[30px]">
                      <td className="sticky left-0 z-10 bg-white px-4 py-2 text-sm text-gray-900 border-r border-gray-300">
                        <div>{user.name}</div>
                        <div className="text-xs text-gray-500">{user.type}</div>
                      </td>
                      {resources.map(resource => {
                        const perm = getPermission(user.id, resource.id);
                        return (
                          <td key={resource.id} className="px-4 py-2 border-r border-gray-200">
                            <div className="flex gap-2 items-center justify-center">
                              <label className="flex items-center gap-1 cursor-pointer">
                                <input
                                  type="checkbox"
                                  checked={perm.read}
                                  onChange={() => togglePermission(user.id, resource.id, 'read')}
                                  className="w-3 h-3 cursor-pointer"
                                />
                                <span className="text-xs text-gray-600">R</span>
                              </label>
                              <label className="flex items-center gap-1 cursor-pointer">
                                <input
                                  type="checkbox"
                                  checked={perm.write}
                                  onChange={() => togglePermission(user.id, resource.id, 'write')}
                                  className="w-3 h-3 cursor-pointer"
                                />
                                <span className="text-xs text-gray-600">W</span>
                              </label>
                              <label className="flex items-center gap-1 cursor-pointer">
                                <input
                                  type="checkbox"
                                  checked={perm.execute}
                                  onChange={() => togglePermission(user.id, resource.id, 'execute')}
                                  className="w-3 h-3 cursor-pointer"
                                />
                                <span className="text-xs text-gray-600">X</span>
                              </label>
                            </div>
                          </td>
                        );
                      })}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <div className="mt-4 text-xs text-gray-500">
              R = Read, W = Write, X = Execute. Changes are logged to the audit stream.
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
