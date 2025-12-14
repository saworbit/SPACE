import { useState } from 'react';
import { ChevronRight, ChevronDown } from 'lucide-react';
import { IAMTable } from './registry/IAMTable';
import { AuditLog } from './registry/AuditLog';
import { StorageInventory } from './registry/StorageInventory';
import { BillingLedger } from './registry/BillingLedger';

type RegistryView = 'iam' | 'audit' | 'storage' | 'billing';

interface NavItem {
  id: string;
  label: string;
  children?: NavItem[];
  view?: RegistryView;
}

const navStructure: NavItem[] = [
  {
    id: 'system',
    label: 'System',
    children: [
      { id: 'audit', label: 'Global Audit Log', view: 'audit' },
      { id: 'storage', label: 'Storage Inventory', view: 'storage' }
    ]
  },
  {
    id: 'identity',
    label: 'Identity & Access',
    children: [
      { id: 'iam', label: 'IAM Management', view: 'iam' }
    ]
  },
  {
    id: 'billing',
    label: 'Billing & Usage',
    children: [
      { id: 'ledger', label: 'Usage Ledger', view: 'billing' }
    ]
  }
];

export function Registry() {
  const [activeView, setActiveView] = useState<RegistryView>('iam');
  const [expandedNodes, setExpandedNodes] = useState<Set<string>>(new Set(['system', 'identity', 'billing']));

  const toggleNode = (id: string) => {
    setExpandedNodes(prev => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  return (
    <div className="w-full h-full bg-white flex">
      {/* Left Navigation */}
      <div className="w-64 bg-gray-50 border-r border-gray-200 flex-shrink-0">
        <div className="p-4 border-b border-gray-200">
          <h1 className="text-gray-900 tracking-tight">THE REGISTER</h1>
          <p className="text-gray-500 text-xs mt-1">Engineer's Logbook</p>
        </div>
        
        <nav className="p-2">
          {navStructure.map(item => (
            <NavNode
              key={item.id}
              item={item}
              expanded={expandedNodes.has(item.id)}
              onToggle={toggleNode}
              onSelect={setActiveView}
              activeView={activeView}
              expandedNodes={expandedNodes}
            />
          ))}
        </nav>
      </div>

      {/* Main Content */}
      <div className="flex-1 flex flex-col overflow-hidden">
        {activeView === 'iam' && <IAMTable />}
        {activeView === 'audit' && <AuditLog />}
        {activeView === 'storage' && <StorageInventory />}
        {activeView === 'billing' && <BillingLedger />}
      </div>
    </div>
  );
}

function NavNode({
  item,
  expanded,
  onToggle,
  onSelect,
  activeView,
  level = 0,
  expandedNodes
}: {
  item: NavItem;
  expanded: boolean;
  onToggle: (id: string) => void;
  onSelect: (view: RegistryView) => void;
  activeView: RegistryView;
  level?: number;
  expandedNodes: Set<string>;
}) {
  const hasChildren = item.children && item.children.length > 0;
  const isActive = item.view === activeView;

  return (
    <div>
      <button
        onClick={() => {
          if (hasChildren) {
            onToggle(item.id);
          }
          if (item.view) {
            onSelect(item.view);
          }
        }}
        className={`w-full flex items-center gap-2 px-3 py-2 text-sm text-left transition-colors ${
          isActive 
            ? 'bg-gray-900 text-white' 
            : 'text-gray-700 hover:bg-gray-100'
        }`}
        style={{ paddingLeft: `${12 + level * 16}px` }}
      >
        {hasChildren && (
          expanded ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />
        )}
        {!hasChildren && <div className="w-4" />}
        <span className={isActive ? '' : ''}>{item.label}</span>
      </button>
      
      {hasChildren && expanded && (
        <div>
          {item.children!.map(child => (
            <NavNode
              key={child.id}
              item={child}
              expanded={expandedNodes.has(child.id)}
              onToggle={onToggle}
              onSelect={onSelect}
              activeView={activeView}
              level={level + 1}
              expandedNodes={expandedNodes}
            />
          ))}
        </div>
      )}
    </div>
  );
}