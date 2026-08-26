use super::*;

impl VerifiedIndex {
    /// Selects the semantic corpus with one adapter derived from the same
    /// compiled filter authority used by lexical retrieval and hydration.
    pub fn semantic_filter_projection(
        &self,
        filters: &EventSearchFilters,
    ) -> Result<SemanticFilterProjection> {
        let compiled = CompiledSearchFilter::compile(filters.clone())?;
        self.semantic_filter_projection_compiled(&compiled)
    }

    pub fn semantic_filter_projection_compiled(
        &self,
        filter: &CompiledSearchFilter,
    ) -> Result<SemanticFilterProjection> {
        validate_event_sort_fast_fields(&self.searcher)?;
        let fields = fields_from_schema(self.searcher.schema())?;
        let semantic_eligibility = Box::new(BooleanQuery::intersection(vec![
            Box::new(TermQuery::new(
                Term::from_field_text(fields.event_type, "message"),
                IndexRecordOption::Basic,
            )),
            Box::new(TermQuery::new(
                Term::from_field_text(fields.role, "user"),
                IndexRecordOption::Basic,
            )),
        ]));
        let source_identity_query = self.source_identity_query(filter.filters(), fields)?;
        let query =
            filtered_event_query(semantic_eligibility, source_identity_query, filter, fields)?;
        let addresses = self
            .searcher
            .search(query.as_ref(), &DocSetCollector)
            .map_err(IndexError::from)?;
        let mut event_ids = HashSet::with_capacity(addresses.len());
        for address in addresses {
            let (event_id, _, _) = core_event_fast_preflight(&self.searcher, address)?;
            if !event_ids.insert(event_id) {
                return Err(IndexError::DuplicateEventIdentity(event_id.to_string()));
            }
        }
        Ok(SemanticFilterProjection {
            generation_id: self.generation_id.clone(),
            event_ids,
        })
    }

    fn source_identity_query(
        &self,
        filters: &EventSearchFilters,
        fields: Fields,
    ) -> Result<Option<Box<dyn Query>>> {
        if !filters.has_source_identity_filter() {
            return Ok(None);
        }
        filters.validate_source_identity_filters()?;
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(fields.provider, "custom"),
                IndexRecordOption::Basic,
            )),
        )];
        if let Some(history_source) = filters.history_source.as_deref() {
            let Some((history_provider_key, history_source_id)) =
                history_source.trim().split_once('/')
            else {
                return Ok(Some(Box::new(EmptyQuery)));
            };
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_provider_key, history_provider_key),
                    IndexRecordOption::Basic,
                )),
            ));
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_source_id, history_source_id),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(provider_key) = filters.provider_key.as_deref().map(str::trim) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_provider_key, provider_key),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(source_id) = filters.source_id.as_deref().map(str::trim) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.custom_source_id, source_id),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        Ok(Some(Box::new(BooleanQuery::new(clauses))))
    }
}
