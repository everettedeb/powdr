use crate::{
    file_writer::BBFiles,
    relation_builder::create_row_type,
    utils::{get_relations_imports, map_with_newline, snake_case},
};

pub trait CircuitBuilder {
    fn create_circuit_builder_hpp(
        &mut self,
        name: &str,
        relations: &[String],
        permutations: &[String],
        all_cols_without_inverses: &[String],
        all_cols: &[String],
        to_be_shifted: &[String],
        all_cols_with_shifts: &[String],
    );
}

fn circuit_hpp_includes(name: &str, relations: &[String], permutations: &[String]) -> String {
    let relation_imports = get_relations_imports(name, relations, permutations);
    format!(
        "
    // AUTOGENERATED FILE
    #pragma once

    #include <vector>
#ifndef __wasm__
    #include <future>
#endif

    #include \"barretenberg/common/constexpr_utils.hpp\"
    #include \"barretenberg/common/throw_or_abort.hpp\"
    #include \"barretenberg/ecc/curves/bn254/fr.hpp\"
    #include \"barretenberg/stdlib_circuit_builders/circuit_builder_base.hpp\"
    #include \"barretenberg/relations/generic_permutation/generic_permutation_relation.hpp\"
    #include \"barretenberg/relations/generic_lookup/generic_lookup_relation.hpp\"
    #include \"barretenberg/honk/proof_system/logderivative_library.hpp\"
    
    #include \"barretenberg/vm/generated/{name}_flavor.hpp\"
    {relation_imports}
"
    )
}

fn get_params() -> &'static str {
    r#"
    const FF gamma = FF::random_element();
    const FF beta = FF::random_element();
    bb::RelationParameters<typename Flavor::FF> params{
        .eta = 0,
        .beta = beta,
        .gamma = gamma,
        .public_input_delta = 0,
        .lookup_grand_product_delta = 0,
        .beta_sqr = 0,
        .beta_cube = 0,
        .eccvm_set_permutation_delta = 0,
    };
    "#
}

impl CircuitBuilder for BBFiles {
    // Create circuit builder
    // Generate some code that can read a commits.bin and constants.bin into data structures that bberg understands
    fn create_circuit_builder_hpp(
        &mut self,
        name: &str,
        relations: &[String],
        permutations: &[String],
        all_cols_without_inverses: &[String],
        all_cols: &[String],
        to_be_shifted: &[String],
        all_cols_with_shifts: &[String],
    ) {
        let includes = circuit_hpp_includes(&snake_case(name), relations, permutations);

        let row_with_all_included = create_row_type(&format!("{name}Full"), all_cols_with_shifts);

        let num_polys = all_cols.len();
        let num_cols = all_cols.len() + to_be_shifted.len();

        // Declare mapping transformations
        let compute_polys_transformation =
            |name: &String| format!("polys.{name}[i] = rows[i].{name};");
        let all_polys_transformation =
            |name: &String| format!("polys.{name}_shift = Polynomial(polys.{name}.shifted());");
        let check_circuit_transformation = |relation_name: &String| {
            format!(
                    "auto {relation_name} = [=]() {{
                        return evaluate_relation.template operator()<{name}_vm::{relation_name}<FF>>(\"{relation_name}\", {name}_vm::get_relation_label_{relation_name});
                    }};
                    ",
                    name = name,
                    relation_name = relation_name
                )
        };
        let check_lookup_transformation = |lookup_name: &String| {
            let lookup_name_upper = lookup_name.to_uppercase();
            format!(
                    "auto {lookup_name} = [=]() {{
                        return evaluate_logderivative.template operator()<{lookup_name}_relation<FF>>(\"{lookup_name_upper}\");
                    }};
                    "
                )
        };

        // When we are running natively, we want check circuit to run as futures; however, futures are not supported in wasm, so we must provide an 
        // alternative codepath that will execute the closures in serial
        let emplace_future_transformation = |relation_name: &String| {
            format!(
                "
                    relation_futures.emplace_back(std::async(std::launch::async, {relation_name}));
                "
            )
        };

        let execute_serial_transformation = |relation_name: &String| {
            format!(
                "
                    {relation_name}();
                "
            )
        };

        // Apply transformations
        let compute_polys_assignemnt =
            map_with_newline(all_cols_without_inverses, compute_polys_transformation);
        let all_poly_shifts = map_with_newline(to_be_shifted, all_polys_transformation);
        let check_circuit_for_each_relation =
            map_with_newline(relations, check_circuit_transformation);
        let check_circuit_for_each_lookup =
            map_with_newline(permutations, check_lookup_transformation);
        
        // With futures
        let emplace_future_relations = map_with_newline(relations, emplace_future_transformation);
        let emplace_future_lookups = map_with_newline(permutations, emplace_future_transformation);
        
        // With threads
        let serial_relations = map_with_newline(relations, execute_serial_transformation);
        let serial_lookups = map_with_newline(permutations, execute_serial_transformation);

        let (params, lookup_check_closure) = if !permutations.is_empty() {
            (get_params(), get_lookup_check_closure())
        } else {
            ("", "".to_owned())
        };
        let relation_check_closure = if !relations.is_empty() {
            get_relation_check_closure()
        } else {
            "".to_owned()
        };


        let circuit_hpp = format!("
{includes}

namespace bb {{

{row_with_all_included};

class {name}CircuitBuilder {{
    public:
        using Flavor = bb::{name}Flavor;
        using FF = Flavor::FF;
        using Row = {name}FullRow<FF>;

        // TODO: template
        using Polynomial = Flavor::Polynomial;
        using ProverPolynomials = Flavor::ProverPolynomials;

        static constexpr size_t num_fixed_columns = {num_cols};
        static constexpr size_t num_polys = {num_polys};
        std::vector<Row> rows;

        void set_trace(std::vector<Row>&& trace) {{ rows = std::move(trace); }}

        ProverPolynomials compute_polynomials() {{
            const auto num_rows = get_circuit_subgroup_size();
            ProverPolynomials polys;

            // Allocate mem for each column
            for (auto& poly : polys.get_all()) {{
                poly = Polynomial(num_rows);
            }}

            for (size_t i = 0; i < rows.size(); i++) {{
                {compute_polys_assignemnt}
            }}

            {all_poly_shifts }

            return polys;
        }}

        [[maybe_unused]] bool check_circuit()
        {{
            {params}

            auto polys = compute_polynomials();
            const size_t num_rows = polys.get_polynomial_size();
    
            {relation_check_closure}

            {lookup_check_closure}

            {check_circuit_for_each_relation}

            {check_circuit_for_each_lookup}

#ifndef __wasm__

            // Evaluate check circuit closures as futures
            std::vector<std::future<bool>> relation_futures;

            {emplace_future_relations}
            {emplace_future_lookups}


            // Wait for lookup evaluations to complete
            for (auto& future : relation_futures) {{
                int result = future.get();
                if (!result) {{
                    return false;
                }}
            }}
#else
            {serial_relations}
            {serial_lookups}

#endif

            return true;
        }}
    

        [[nodiscard]] size_t get_num_gates() const {{ return rows.size(); }}

        [[nodiscard]] size_t get_circuit_subgroup_size() const
        {{
            const size_t num_rows = get_num_gates();
            const auto num_rows_log2 = static_cast<size_t>(numeric::get_msb64(num_rows));
            size_t num_rows_pow2 = 1UL << (num_rows_log2 + (1UL << num_rows_log2 == num_rows ? 0 : 1));
            return num_rows_pow2;
        }}


}};
}}
        ");

        self.write_file(
            &self.circuit,
            &format!("{}_circuit_builder.hpp", snake_case(name)),
            &circuit_hpp,
        );
    }
}

fn get_lookup_check_closure() -> String {
    "
            const auto evaluate_logderivative = [&]<typename LogDerivativeSettings>(const std::string& lookup_name) {

                // Check the logderivative relation
                bb::compute_logderivative_inverse<
                    Flavor,
                    LogDerivativeSettings>(
                    polys, params, num_rows);
        
                typename LogDerivativeSettings::SumcheckArrayOfValuesOverSubrelations
                    lookup_result;

                for (auto& r : lookup_result) {
                    r = 0;
                }
                for (size_t i = 0; i < num_rows; ++i) {
                    LogDerivativeSettings::accumulate(lookup_result, polys.get_row(i), params, 1);
                }
                for (auto r : lookup_result) {
                    if (r != 0) {
                        throw_or_abort(format(\"Lookup \", lookup_name, \" failed.\"));
                        return false;
                    }
                }
                return true;
            };
            ".to_string()
}

fn get_relation_check_closure() -> String {
    "
            const auto evaluate_relation = [&]<typename Relation>(const std::string& relation_name,
                                                                    std::string (*debug_label)(int)) {
                typename Relation::SumcheckArrayOfValuesOverSubrelations result;
                for (auto& r : result) {
                    r = 0;
                }
                constexpr size_t NUM_SUBRELATIONS = result.size();
    
                for (size_t i = 0; i < num_rows; ++i) {
                    Relation::accumulate(result, polys.get_row(i), {}, 1);
    
                    bool x = true;
                    for (size_t j = 0; j < NUM_SUBRELATIONS; ++j) {
                        if (result[j] != 0) {
                            std::string row_name = debug_label(static_cast<int>(j));
                            throw_or_abort(
                                format(\"Relation \", relation_name, \", subrelation index \", row_name, \" failed at row \", i));
                            x = false;
                        }
                    }
                    if (!x) {
                        return false;
                    }
                }
                return true;
            };
    ".to_string()
}
