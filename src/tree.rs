//! Tree model mirroring `awsService/ecs/model.ts` in aws-toolkit-vscode:
//! Region > ECS > Cluster > Service > Container.

use crate::aws::{Cluster, Service, Task};

#[derive(Debug, Clone)]
pub enum Kind {
    Region { name: String },
    Ecs,
    Cluster(Cluster),
    Service(Service),
    Container {
        name: String,
        cluster: Cluster,
        service: Service,
        /// Tasks of the parent service, RUNNING first.
        tasks: Vec<Task>,
    },
    /// One running instance of a container (a task), the leaf of the tree.
    Task {
        task: Task,
        container: String,
        cluster: Cluster,
        service: Service,
    },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: Kind,
    pub expanded: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// `None` = not loaded yet; `Some(vec![])` = loaded, empty.
    pub children: Option<Vec<Node>>,
}

impl Node {
    pub fn new(kind: Kind) -> Self {
        // Containers already know their tasks, so their children are built eagerly.
        let children = match &kind {
            Kind::Container { name, cluster, service, tasks } => Some(
                tasks
                    .iter()
                    .map(|task| {
                        Node::new(Kind::Task {
                            task: task.clone(),
                            container: name.clone(),
                            cluster: cluster.clone(),
                            service: service.clone(),
                        })
                    })
                    .collect(),
            ),
            Kind::Task { .. } => Some(vec![]),
            _ => None,
        };
        Node { kind, expanded: false, loading: false, error: None, children }
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self.kind, Kind::Task { .. })
    }

    pub fn label(&self) -> String {
        match &self.kind {
            Kind::Region { name } => name.clone(),
            Kind::Ecs => "ECS".into(),
            Kind::Cluster(c) => c.name.clone(),
            Kind::Service(s) => s.name.clone(),
            Kind::Container { name, .. } => name.clone(),
            Kind::Task { task, .. } => task.id().to_string(),
        }
    }

    /// Secondary text shown dimmed after the label (the reference shows service status).
    pub fn description(&self) -> Option<String> {
        match &self.kind {
            Kind::Service(s) => Some(s.status.clone()),
            Kind::Task { task, container, .. } => Some(if task.last_status == "RUNNING" && !task.exec_ready_for(container) {
                format!("{}  no exec agent", task.last_status)
            } else if task.last_status != task.desired_status {
                format!("{} → {}", task.last_status, task.desired_status)
            } else {
                task.last_status.clone()
            }),
            _ => None,
        }
    }

    /// Whether this node is a usable target for `execute-command`.
    pub fn exec_ready(&self) -> Option<bool> {
        match &self.kind {
            Kind::Task { task, container, .. } => Some(task.last_status == "RUNNING" && task.exec_ready_for(container)),
            _ => None,
        }
    }

    pub fn icon(&self) -> &'static str {
        match &self.kind {
            Kind::Region { .. } => "🌐",
            Kind::Ecs => "  ",
            Kind::Cluster(_) => "⬡ ",
            Kind::Service(_) => "▣ ",
            Kind::Container { .. } => "▤ ",
            Kind::Task { .. } => "▪ ",
        }
    }

    pub fn placeholder(&self) -> &'static str {
        match &self.kind {
            Kind::Ecs => "[No Clusters found]",
            Kind::Cluster(_) => "[No Services found]",
            Kind::Service(_) => "[No Containers found]",
            Kind::Container { .. } => "[No tasks running]",
            _ => "[Empty]",
        }
    }
}

/// A row in the flattened, visible tree.
#[derive(Debug, Clone)]
pub struct Row {
    pub path: Vec<usize>,
    pub depth: usize,
    /// Set for synthetic rows (placeholder / error / loading) that have no node.
    pub message: Option<String>,
}

pub struct Tree {
    pub root: Node,
}

impl Tree {
    pub fn new(region: String) -> Self {
        let mut root = Node::new(Kind::Region { name: region });
        root.expanded = true;
        let mut ecs = Node::new(Kind::Ecs);
        ecs.expanded = true;
        root.children = Some(vec![ecs]);
        Tree { root }
    }

    pub fn get(&self, path: &[usize]) -> Option<&Node> {
        let mut n = &self.root;
        for &i in path {
            n = n.children.as_ref()?.get(i)?;
        }
        Some(n)
    }

    pub fn get_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        let mut n = &mut self.root;
        for &i in path {
            n = n.children.as_mut()?.get_mut(i)?;
        }
        Some(n)
    }

    /// Paths of the nodes whose last load failed.
    pub fn failed(&self) -> Vec<Vec<usize>> {
        fn walk(node: &Node, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            if node.error.is_some() {
                out.push(path.clone());
            }
            for (i, c) in node.children.iter().flatten().enumerate() {
                path.push(i);
                walk(c, path, out);
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut out);
        out
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        self.walk(&self.root, &mut Vec::new(), 0, &mut out);
        out
    }

    fn walk(&self, node: &Node, path: &mut Vec<usize>, depth: usize, out: &mut Vec<Row>) {
        out.push(Row { path: path.clone(), depth, message: None });
        if !node.expanded {
            return;
        }
        if node.loading {
            out.push(Row { path: path.clone(), depth: depth + 1, message: Some("⟳ Loading…".into()) });
            return;
        }
        if let Some(e) = &node.error {
            out.push(Row { path: path.clone(), depth: depth + 1, message: Some(format!("Error: {e}")) });
            return;
        }
        match &node.children {
            Some(children) if children.is_empty() => {
                out.push(Row { path: path.clone(), depth: depth + 1, message: Some(node.placeholder().into()) });
            }
            Some(children) => {
                for (i, c) in children.iter().enumerate() {
                    path.push(i);
                    self.walk(c, path, depth + 1, out);
                    path.pop();
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn cluster(name: &str) -> Node {
        Node::new(Kind::Cluster(Cluster { arn: format!("arn:aws:ecs:eu-west-1:1:cluster/{name}"), name: name.into() }))
    }

    #[test]
    fn failed_lists_nodes_with_errors_at_any_depth() {
        let mut tree = Tree::new("eu-west-1".into());
        let mut bad = cluster("bad");
        bad.error = Some("Token has expired".into());
        tree.get_mut(&[0]).unwrap().children = Some(vec![cluster("ok"), bad]);
        assert_eq!(tree.failed(), vec![vec![0, 1]]);

        tree.get_mut(&[0]).unwrap().error = Some("Token has expired".into());
        assert_eq!(tree.failed(), vec![vec![0], vec![0, 1]]);
    }
}
