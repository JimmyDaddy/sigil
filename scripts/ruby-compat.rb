# frozen_string_literal: true

# Keep repository checks runnable on the system Ruby shipped with older macOS.
unless Enumerable.method_defined?(:filter_map)
  module Enumerable
    def filter_map
      return enum_for(__method__) unless block_given?

      each_with_object([]) do |item, values|
        value = yield(item)
        values << value if value
      end
    end
  end
end

unless Enumerable.method_defined?(:tally)
  module Enumerable
    def tally
      each_with_object(Hash.new(0)) { |item, counts| counts[item] += 1 }
    end
  end
end
