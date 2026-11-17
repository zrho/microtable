


```rust
#[derive(Debug, Clone)]
pub struct Person {
    pub name: String,
    pub email: String,
}

microtable::table! {
    #[derive(Debug, Clone)]
    pub struct PersonTable([Person]);

    #[id]
    pub struct PersonId(u32);
    
    #[index(unique, cached)]
    fn email(person: &Person) -> String {
        person.email.clone()
    }

    #[index(many)]
    fn name(person: &Person) -> &str {
        person.name.as_str()
    }
}
```

```
#[derive(Debug, Clone)]
pub enum Node {
    Leaf(String),
    Inner(NodeId, NodeId)
}

microtable::table! {
    #[derive(Debug, Clone)]
    pub struct BinaryForest([Node]);

    #[id]
    pub struct NodeId(u32);

    #[index(unique, optional)]
    fn leaf(node: &Node) -> Option<&str> {
        match node {
            Node::Leaf(leaf) => Some(leaf.as_str()),
            Node::Inner(_, _) => None,
        }
    }
}

```
