use oxc_ast::AstKind;
use quickjs_frontend::{FrontendOptions, ParseMode, with_parsed_program};

const OPERATOR_COUNT: usize = 20_000;

#[derive(Clone, Copy)]
enum ChainKind {
    Binary,
    Logical,
}

#[test]
fn accepts_twenty_thousand_left_deep_additions() {
    assert_long_expression_chain(" + operand", ChainKind::Binary);
}

#[test]
fn accepts_twenty_thousand_left_deep_logical_operations() {
    assert_long_expression_chain(" && operand", ChainKind::Logical);
}

fn assert_long_expression_chain(operator_and_operand: &str, chain_kind: ChainKind) {
    let mut source = String::with_capacity(
        "let operand = true;\nlet result = operand;".len()
            + operator_and_operand.len() * OPERATOR_COUNT,
    );
    source.push_str("let operand = true;\nlet result = operand");
    for _ in 0..OPERATOR_COUNT {
        source.push_str(operator_and_operand);
    }
    source.push(';');

    with_parsed_program(&source, FrontendOptions::new(ParseMode::Script), |unit| {
        let scoping = unit.scoping();
        assert_eq!(scoping.references_len(), OPERATOR_COUNT + 1);
        assert!(scoping.root_unresolved_references().is_empty());

        let resolved_counts = scoping
            .symbol_names()
            .zip(scoping.resolved_references())
            .map(|(name, references)| (name, references.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            resolved_counts,
            vec![("operand", OPERATOR_COUNT + 1), ("result", 0)]
        );

        let nodes = unit.semantic().nodes();
        let chain_node_ids = nodes
            .iter_enumerated()
            .filter_map(|(node_id, node)| {
                let is_expected_kind = match chain_kind {
                    ChainKind::Binary => {
                        matches!(node.kind(), AstKind::BinaryExpression(_))
                    }
                    ChainKind::Logical => {
                        matches!(node.kind(), AstKind::LogicalExpression(_))
                    }
                };
                is_expected_kind.then_some(node_id)
            })
            .collect::<Vec<_>>();

        assert_eq!(chain_node_ids.len(), OPERATOR_COUNT);
        for pair in chain_node_ids.windows(2) {
            assert_eq!(nodes.parent_id(pair[1]), pair[0]);
        }
    })
    .expect("QuickJS-compatible long expression chain must be stack safe");
}
